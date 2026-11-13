//! Currency suffixes and `Money` constructors (hir-host.md B4, slice HB2).
//!
//! `12.34.GBP` is `Money<GBP>(12.34)`: a numeric literal followed by the name of a type in scope lowers to a call of
//! the `core.money` constructor, with the literal kept exact. The currency is a type argument, so the call expression
//! carries the type `Money<GBP>`. Whether the named type implements `Currency` is checked by the type checker, because
//! the `impl` may follow the use.

use num_bigint::BigInt;
use orna_syntax_ast::SourceSpan as SyntaxSpan;
use orna_syntax_ast::{Expr as AstExpr, LiteralKind};

use super::BodyState;
use super::declare::{Builder, PendingKind};
use super::exprs::{lower_expr, push_typed};
use crate::{
    Arg, ArgValue, Call, Callee, DefId, DefKind, ExprId, ExprKind, Idx, Lit, Name, NameTarget,
    ParamIdx, TyKind, UseContext,
};

/// The `core.money` module path, where `Money` is declared.
fn money_module_path() -> crate::ModulePath {
    crate::ModulePath::new([Name::new("core"), Name::new("money")])
}

/// The `Money` nominal type of `core.money`, when that module is loaded.
pub(crate) fn money_def(builder: &Builder) -> Option<DefId> {
    let module = *builder.by_path.get(&money_module_path())?;
    builder.program.modules[module]
        .exports
        .get(&Name::new("Money"))
        .copied()
}

/// Whether `def` is a nominal type declaration, the only kind of definition a currency suffix may name.
fn is_nominal_type(builder: &Builder, def: DefId) -> bool {
    matches!(
        builder
            .pending
            .get(def.index())
            .map(|pending| &pending.kind),
        Some(PendingKind::Built(DefKind::Type(_)))
    )
}

/// Lowers `literal.Name` when `Name` is a type in scope, as `Money<Name>(literal)`. `None` when the base is not a
/// numeric literal, the name does not resolve to a type, or `core.money` is not loaded; the caller then reports the
/// name as it does today.
pub(crate) fn lower_currency_suffix(
    builder: &mut Builder,
    state: &mut BodyState,
    base: &AstExpr,
    name: &str,
    span: &SyntaxSpan,
) -> Option<ExprId> {
    if !matches!(
        base,
        AstExpr::Literal {
            kind: LiteralKind::Integer | LiteralKind::Decimal | LiteralKind::Float,
            ..
        }
    ) {
        return None;
    }
    let currency_name = Name::new(name);
    let NameTarget::Def(currency) = builder.resolve_bare(state.module, &currency_name) else {
        return None;
    };
    if !is_nominal_type(builder, currency) {
        return None;
    }
    let money = money_def(builder)?;
    let name_span = SyntaxSpan::new(span.end.saturating_sub(name.len()), span.end);
    super::record_body_use(
        &mut builder.program,
        state.module,
        state.id,
        name_span.clone(),
        currency_name,
        UseContext::Value,
        NameTarget::Def(currency),
    );
    // The literal is charged with the field access that names the currency, as a built-in postfix is (two steps).
    let literal = lower_expr(builder, state, base);
    state.exprs[literal].weight = 2;
    // `core.money` takes a `Decimal` amount, so an integer base is the exact decimal of the same value.
    if let ExprKind::Lit(Lit::Int(value)) = state.exprs[literal].kind.clone() {
        state.exprs[literal].kind = ExprKind::Lit(Lit::Decimal {
            coefficient: value,
            exponent: BigInt::from(0),
        });
        state.exprs[literal].ty = builder.program.tys.intern(TyKind::Decimal);
    }
    let currency_ty = builder
        .program
        .tys
        .intern(TyKind::Adt(currency, Box::new([])));
    let money_ty = builder
        .program
        .tys
        .intern(TyKind::Adt(money, Box::new([currency_ty])));
    let call = Call {
        callee: Callee::Ctor { def: money },
        callee_span: name_span,
        args: Box::new([Arg {
            slot: ParamIdx::new(0),
            value: ArgValue::Expr(literal),
        }]),
        piped: false,
    };
    Some(push_typed(
        builder,
        state,
        ExprKind::Call(Box::new(call)),
        span.clone(),
        money_ty,
    ))
}
