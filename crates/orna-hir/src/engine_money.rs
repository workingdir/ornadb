//! The engine's currency predicate (hir-host.md B3, open point Q-B1).
//!
//! A protocol is the currency protocol when it is `core.money.Currency`, or when it is named `Currency` and its
//! members are exactly `static code: Str` and `static minor_digits: Int` (the ORNA-MONEY-004 contract). A user
//! `protocol Currency` with those members is therefore accepted, and one with any other member (for example
//! `static symbol`) is not. Typeck reads this one predicate to decide whether `C impl Currency` holds for a type
//! argument, so the owner can change the Q-B1 decision by editing `is_currency_protocol` alone.

use crate::{DefId, DefKind, Program, Ty, TyKind};

/// The name of the currency protocol, in the engine's `core.money` module and in a user's own declaration.
const CURRENCY: &str = "Currency";

/// The name of the `core.money` value type whose amount is one `Decimal` in one currency.
const MONEY: &str = "Money";

/// Whether `protocol` is the currency protocol under the Q-B1 default.
#[must_use]
pub fn is_currency_protocol(program: &Program, protocol: DefId) -> bool {
    let definition = &program.defs[protocol];
    let DefKind::Protocol(declared) = &definition.kind else {
        return false;
    };
    if definition.name.as_str() != CURRENCY {
        return false;
    }
    if program.modules[definition.module].path.to_string() == "core.money" {
        return true;
    }
    if !declared.params.is_empty() || declared.members.len() != 2 {
        return false;
    }
    let mut code = false;
    let mut minor_digits = false;
    for member in &declared.members {
        let member = &program.defs[*member];
        let DefKind::Field(field) = &member.kind else {
            return false;
        };
        match (member.name.as_str(), program.tys.kind(field.ty)) {
            ("code", Some(TyKind::Str)) => code = true,
            ("minor_digits", Some(TyKind::Int)) => minor_digits = true,
            _ => return false,
        }
    }
    code && minor_digits
}

/// Whether the static type `ty` implements the currency protocol: a nominal type with an implementation of a
/// protocol that `is_currency_protocol` accepts.
#[must_use]
pub fn is_currency_type(program: &Program, ty: Ty) -> bool {
    let Some(TyKind::Adt(owner, _)) = program.tys.kind(ty) else {
        return false;
    };
    program
        .instances
        .get(owner)
        .into_iter()
        .flatten()
        .any(|implementation| {
            matches!(
                &program.defs[*implementation].kind,
                DefKind::Impl(declared) if declared.protocol.is_some_and(|protocol| is_currency_protocol(program, protocol))
            )
        })
}

/// The type arguments of `ty` that break a currency bound its nominal declaration states, such as the `C` of
/// `Money<C impl Currency>`. A type parameter argument is not judged here: its own bound is checked where it is
/// instantiated.
#[must_use]
pub fn currency_bound_failures(program: &Program, ty: Ty) -> Vec<Ty> {
    let mut failures = Vec::new();
    collect_failures(program, ty, &mut failures);
    failures
}

fn collect_failures(program: &Program, ty: Ty, failures: &mut Vec<Ty>) {
    match program.tys.kind(ty) {
        Some(TyKind::Adt(owner, arguments)) => {
            let params = match &program.defs[*owner].kind {
                DefKind::Type(declared) => declared.params.as_slice(),
                _ => &[],
            };
            for (index, argument) in arguments.iter().enumerate() {
                let bounded = params.get(index).is_some_and(|param| {
                    param
                        .bounds
                        .iter()
                        .any(|bound| is_currency_protocol(program, bound.protocol))
                });
                let is_param = matches!(program.tys.kind(*argument), Some(TyKind::Param(_)));
                if bounded && !is_param && !is_currency_type(program, *argument) {
                    failures.push(*argument);
                }
                collect_failures(program, *argument, failures);
            }
        }
        Some(TyKind::Tuple(items)) => {
            for item in items.iter() {
                collect_failures(program, *item, failures);
            }
        }
        _ => {}
    }
}

/// Whether `def` is `core.money.Money`, the value whose amount is a `Decimal` in one currency (hir-host.md B6).
#[must_use]
pub fn is_money_def(program: &Program, def: DefId) -> bool {
    let definition = &program.defs[def];
    definition.name.as_str() == MONEY
        && program.modules[definition.module].path.to_string() == "core.money"
}

/// The currency type argument of `ty` when `ty` is `core.money.Money<C>`.
#[must_use]
pub fn money_currency(program: &Program, ty: Ty) -> Option<Ty> {
    let Some(TyKind::Adt(owner, args)) = program.tys.kind(ty) else {
        return None;
    };
    if !is_money_def(program, *owner) {
        return None;
    }
    args.first().copied()
}
