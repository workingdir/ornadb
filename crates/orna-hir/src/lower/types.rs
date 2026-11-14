//! `TypeExpr` to `Ty` (design section 2.3).
//!
//! Stage H1 records the type a declaration writes; it does not infer or
//! unify. A name that resolves to a declared type becomes
//! [`crate::TyKind::Adt`] with the declaration's identity, a name that does not
//! resolve becomes [`crate::TyKind::Error`] with one diagnostic, and every type
//! name is recorded in the module's name index so the language server and the
//! resolution comparison see it.
//!
//! Dimension products (`ORNA-PRODUCT`) are dimensions, not types; decision D8
//! is settled by the type pass of stage H2, so lowering keeps the operands'
//! common base rather than inventing an algebra here.

use orna_syntax_ast::TypeExpr;

use super::declare::{Builder, UnitLookup};
use crate::{DefId, EffectRow, FnTy, ModuleId, Name, NameTarget, ParamIdx, Ty, TyKind};

/// A language-defined type constructor with no declaration.
fn builtin(name: &str) -> Option<&'static str> {
    Some(match name {
        "Bool" => "Bool",
        "Int" => "Int",
        "Decimal" => "Decimal",
        "Float" => "Float",
        "Str" | "Text" => "Str",
        "Blob" | "Bytes" => "Blob",
        "Uuid" => "Uuid",
        "Date" => "Date",
        "Instant" => "Instant",
        "Duration" => "Duration",
        "Period" => "Period",
        "Digest" => "Digest",
        "Path" => "Path",
        "TimeZone" => "TimeZone",
        "Locale" => "Locale",
        "PresentContext" => "PresentContext",
        "PresentTree" => "PresentTree",
        "Unit" | "Nothing" => "Unit",
        "Never" => "Never",
        _ => return None,
    })
}

/// Lowers one written type in the module `module` and generics `generics`.
pub(crate) fn lower_type(
    builder: &mut Builder,
    module: ModuleId,
    generics: &[Name],
    expr: &TypeExpr,
) -> Ty {
    match expr {
        TypeExpr::Name {
            path,
            arguments,
            span,
        } => lower_named(builder, module, generics, path, arguments, span),
        TypeExpr::Optional { inner, .. } => {
            let inner = lower_type(builder, module, generics, inner);
            builder.program.tys.intern(TyKind::Option(inner))
        }
        TypeExpr::List { inner, .. } => {
            let inner = lower_type(builder, module, generics, inner);
            builder.program.tys.intern(TyKind::List(inner))
        }
        TypeExpr::Tuple { elements, .. } => {
            let elements = elements
                .iter()
                .map(|element| lower_type(builder, module, generics, element))
                .collect::<Vec<_>>();
            builder
                .program
                .tys
                .intern(TyKind::Tuple(elements.into_boxed_slice()))
        }
        TypeExpr::Record { fields, .. } => {
            let mut fields = fields
                .iter()
                .map(|(name, ty, _)| (Name::new(name), lower_type(builder, module, generics, ty)))
                .collect::<Vec<_>>();
            fields.sort_by(|left, right| left.0.cmp(&right.0));
            builder
                .program
                .tys
                .intern(TyKind::Record(fields.into_boxed_slice()))
        }
        TypeExpr::Function {
            parameters, result, ..
        } => {
            let params = parameters
                .iter()
                .map(|parameter| lower_type(builder, module, generics, parameter))
                .collect::<Vec<_>>();
            let ret = lower_type(builder, module, generics, result);
            builder.program.tys.intern(TyKind::Fn(FnTy {
                params: params.into_boxed_slice(),
                ret,
                effects: EffectRow::Hidden,
            }))
        }
        TypeExpr::Product { lhs, op, rhs, .. } => {
            lower_product(builder, module, generics, lhs, op, rhs)
        }
    }
}

/// Lowers a named type, with or without arguments.
fn lower_named(
    builder: &mut Builder,
    module: ModuleId,
    generics: &[Name],
    path: &[String],
    arguments: &[TypeExpr],
    span: &orna_syntax_ast::SourceSpan,
) -> Ty {
    if let Some(ty) = lower_quantity(builder, module, generics, path, arguments, span) {
        return ty;
    }
    let args = arguments
        .iter()
        .map(|argument| lower_type(builder, module, generics, argument))
        .collect::<Vec<_>>();
    if path.len() == 1 {
        let spelling = path[0].as_str();
        if let Some(ty) = lower_builtin(builder, module, spelling, span) {
            return ty;
        }
        let name = Name::new(spelling);
        if let Some(index) = generics.iter().position(|generic| *generic == name) {
            return builder
                .program
                .tys
                .intern(TyKind::Param(ParamIdx::new(index as u32)));
        }
        if spelling == "Self"
            && let Some(self_ty) = builder.self_ty
        {
            return self_ty;
        }
        let target = builder.resolve_bare(module, &name);
        if !matches!(target, NameTarget::Def(_))
            && let Some(inner) = language_constructor(&name, &args)
        {
            return builder.program.tys.intern(inner);
        }
        super::record_use(
            &mut builder.program,
            module,
            span.clone(),
            name.clone(),
            crate::UseContext::Type,
            target,
        );
        return match target {
            NameTarget::Def(def) => nominal(builder, def, args),
            NameTarget::Ambiguous => {
                builder.diagnose(
                    module,
                    crate::diag::AMBIGUOUS,
                    format!("`{name}` is ambiguous"),
                    span.clone(),
                );
                builder.program.tys.error()
            }
            NameTarget::Intrinsic | NameTarget::Local(_) | NameTarget::Module(_) => {
                builder.program.tys.error()
            }
            NameTarget::Unresolved => {
                builder.diagnose(
                    module,
                    if spelling == "Result" {
                        crate::diag::LEGACY_RESULT
                    } else {
                        crate::diag::UNRESOLVED
                    },
                    if spelling == "Result" {
                        "Result/Ok/Err control plumbing was removed; return the success type directly".to_owned()
                    } else {
                        format!("`{name}` is not a declared type")
                    },
                    span.clone(),
                );
                builder.program.tys.error()
            }
        };
    }
    let segments = path
        .iter()
        .map(|segment| Name::new(segment))
        .collect::<Vec<_>>();
    let target = builder.resolve_path(module, &segments);
    let last = segments.last().cloned().unwrap_or_else(|| Name::new(""));
    match target {
        crate::resolve::PathKind::Def(def) => {
            super::record_use(
                &mut builder.program,
                module,
                span.clone(),
                last,
                crate::UseContext::Type,
                NameTarget::Def(def),
            );
            nominal(builder, def, args)
        }
        _ => {
            super::record_use(
                &mut builder.program,
                module,
                span.clone(),
                last.clone(),
                crate::UseContext::Type,
                NameTarget::Unresolved,
            );
            builder.diagnose(
                module,
                crate::diag::UNRESOLVED,
                format!("`{}` is not a declared type", last),
                span.clone(),
            );
            builder.program.tys.error()
        }
    }
}
/// Lowers a numeric type applied to a unit, `Decimal<kWh>` or `Quantity<Decimal, kWh>` (units.md section 3.4), to
/// [`TyKind::Quantity`] in that unit's key. `None` when the written type has another form, or names no unit, so the
/// generic path lowers it. The unit is resolved first, so that path records no use twice. The key is read from the
/// unit table being evaluated: lowering runs after unit evaluation, and the program's table is only filled when
/// lowering finishes. A unit whose definition is broken has no key; its diagnostic is already recorded.
fn lower_quantity(
    builder: &mut Builder,
    module: ModuleId,
    generics: &[Name],
    path: &[String],
    arguments: &[TypeExpr],
    span: &orna_syntax_ast::SourceSpan,
) -> Option<Ty> {
    let [head] = path else {
        return None;
    };
    let (number_expr, unit) = match (head.as_str(), arguments) {
        ("Int" | "Decimal" | "Float", [unit]) => (None, unit),
        ("Quantity", [number, unit]) => (Some(number), unit),
        _ => return None,
    };
    let TypeExpr::Name {
        path: unit_path,
        arguments: unit_arguments,
        span: unit_span,
    } = unit
    else {
        return None;
    };
    let [unit_name] = unit_path.as_slice() else {
        return None;
    };
    if !unit_arguments.is_empty() {
        return None;
    }
    let name = Name::new(unit_name);
    let def = match builder.resolve_unit(module, &name) {
        UnitLookup::Found(def) => def,
        UnitLookup::Ambiguous(_) => {
            builder.diagnose(
                module,
                crate::diag::AMBIGUOUS,
                format!("unit `{name}` is ambiguous"),
                unit_span.clone(),
            );
            return Some(builder.program.tys.error());
        }
        UnitLookup::None => return None,
    };
    let number = match number_expr {
        None => lower_builtin(builder, module, head, span)?,
        Some(number) => lower_type(builder, module, generics, number),
    };
    if !matches!(
        builder.program.tys.kind(number),
        Some(TyKind::Int | TyKind::Decimal | TyKind::Float)
    ) {
        return Some(builder.program.tys.error());
    }
    super::record_use(
        &mut builder.program,
        module,
        unit_span.clone(),
        name,
        crate::UseContext::Type,
        NameTarget::Def(def),
    );
    // A unit with no key is one the evaluator did not key (a broken definition, already diagnosed, or an affine
    // unit such as core `C`, which the evaluator does not key yet). Its number type stands, so the written type
    // lowers as before with no further diagnostic.
    Some(match builder.unit_eval.table.unit_key(def) {
        Some(key) => builder.program.tys.intern(TyKind::Quantity(number, key)),
        None => number,
    })
}

/// The unit key of a written unit name such as `kWh`, or `None` when the written type is not a bare unit name or the
/// name does not resolve to a unit with a key.
fn unit_operand(
    builder: &mut Builder,
    module: ModuleId,
    expr: &TypeExpr,
) -> Option<crate::ids::UnitKeyId> {
    let TypeExpr::Name {
        path, arguments, ..
    } = expr
    else {
        return None;
    };
    let [name] = path.as_slice() else {
        return None;
    };
    if !arguments.is_empty() {
        return None;
    }
    let lookup = builder.resolve_unit(module, &Name::new(name));
    let super::declare::UnitLookup::Found(unit) = lookup else {
        return None;
    };
    builder.unit_eval.table.unit_key(unit)
}

/// The `Rate` library type of `core.rate`, when that module is loaded.
fn rate_def(builder: &Builder) -> Option<DefId> {
    let path = crate::ModulePath::new([Name::new("core"), Name::new("rate")]);
    let module = *builder.by_path.get(&path)?;
    builder.program.modules[module]
        .exports
        .get(&Name::new("Rate"))
        .copied()
}

/// Lowers `lhs * rhs` or `lhs / rhs` in type position (ORNA-MONEY-003, units.md section 8.3).
///
/// `Money<C> / U` with a bare unit `U` is the library type `Rate<C, Quantity<Decimal, U>>`. A product whose right
/// operand is a bare unit combines that unit's key with the left operand's unit key; the result is the left operand's
/// numeric type in the combined key, or the plain numeric type when the combined key is dimensionless. Any other
/// product keeps the left operand, as H1 did.
fn lower_product(
    builder: &mut Builder,
    module: ModuleId,
    generics: &[Name],
    lhs: &TypeExpr,
    op: &str,
    rhs: &TypeExpr,
) -> Ty {
    let left = lower_type(builder, module, generics, lhs);
    let Some(right_key) = unit_operand(builder, module, rhs) else {
        return left;
    };
    if op == "/"
        && let Some(TyKind::Adt(owner, args)) = builder.program.tys.kind(left).cloned()
        && super::currency::money_def(builder) == Some(owner)
        && let Some(currency) = args.first().copied()
        && let Some(rate) = rate_def(builder)
    {
        let number = builder.program.tys.intern(TyKind::Decimal);
        let per_unit = builder
            .program
            .tys
            .intern(TyKind::Quantity(number, right_key));
        return builder
            .program
            .tys
            .intern(TyKind::Adt(rate, Box::new([currency, per_unit])));
    }
    let (value, left_key) = match builder.program.tys.kind(left).cloned() {
        Some(TyKind::Quantity(value, key)) => (value, key),
        Some(TyKind::Int | TyKind::Float | TyKind::Decimal) => {
            let dimensionless = builder
                .unit_eval
                .table
                .intern(crate::units::UnitKey::dimensionless());
            (left, dimensionless)
        }
        _ => return left,
    };
    let left_unit = builder.unit_eval.table.key(left_key).clone();
    let right_unit = builder.unit_eval.table.key(right_key).clone();
    let combined = if op == "*" {
        left_unit.mul(&right_unit)
    } else {
        left_unit.div(&right_unit)
    };
    let Ok(combined) = combined else {
        return left;
    };
    if combined.is_dimensionless() {
        return value;
    }
    let key = builder.unit_eval.table.intern(combined);
    builder.program.tys.intern(TyKind::Quantity(value, key))
}

/// Lowers a builtin scalar spelling and records its use; `None` when the spelling is not builtin.
fn lower_builtin(
    builder: &mut Builder,
    module: ModuleId,
    spelling: &str,
    span: &orna_syntax_ast::SourceSpan,
) -> Option<Ty> {
    let scalar = builtin(spelling)?;
    super::record_use(
        &mut builder.program,
        module,
        span.clone(),
        Name::new(spelling),
        crate::UseContext::Type,
        NameTarget::Unresolved,
    );
    Some(match scalar {
        "Bool" => builder.program.tys.intern(TyKind::Bool),
        "Int" => builder.program.tys.intern(TyKind::Int),
        "Decimal" => builder.program.tys.intern(TyKind::Decimal),
        "Float" => builder.program.tys.intern(TyKind::Float),
        "Str" => builder.program.tys.intern(TyKind::Str),
        "Blob" => builder.program.tys.intern(TyKind::Blob),
        "Uuid" => builder.program.tys.intern(TyKind::Uuid),
        "Date" => builder.program.tys.intern(TyKind::Date),
        "Instant" => builder.program.tys.intern(TyKind::Instant),
        "Duration" => builder.program.tys.intern(TyKind::Duration),
        "Period" => builder.program.tys.intern(TyKind::Period),
        "Digest" => builder.program.tys.intern(TyKind::Digest),
        "Path" => builder.program.tys.intern(TyKind::Path),
        "TimeZone" => builder.program.tys.intern(TyKind::TimeZone),
        "Locale" => builder.program.tys.intern(TyKind::Locale),
        "PresentContext" => builder.program.tys.intern(TyKind::PresentContext),
        "PresentTree" => builder.program.tys.intern(TyKind::PresentTree),
        "Unit" => builder.program.tys.intern(TyKind::Unit),
        "Never" => builder.program.tys.intern(TyKind::Never),
        _ => builder.program.tys.error(),
    })
}

/// The language's own generic type constructors.
fn language_constructor(name: &Name, args: &[Ty]) -> Option<TyKind> {
    let first = args.first().copied()?;
    Some(match (name.as_str(), args.len()) {
        ("Option", 1) => TyKind::Option(first),
        ("List", 1) => TyKind::List(first),
        ("Set", 1) => TyKind::Set(first),
        ("Map", 2) => TyKind::Map(first, args[1]),
        ("Range", 1) => TyKind::Range(first),
        ("Relation", 1) => TyKind::Relation(first),
        ("Stream", 1) => TyKind::Stream(first),
        _ => return None,
    })
}

/// The type of a declared nominal, enum, table or alias.
///
/// Lowering builds definitions in `Builder::pending` and writes them into the
/// program only in `Builder::finish`, so this asks the pending table: reading
/// `program.defs` here would answer "not a type" for every declaration and turn
/// each written nominal type into the diagnostic type.
fn nominal(builder: &mut Builder, def: DefId, args: Vec<Ty>) -> Ty {
    if builder.is_nominal(def) {
        builder
            .program
            .tys
            .intern(TyKind::Adt(def, args.into_boxed_slice()))
    } else {
        builder.program.tys.error()
    }
}
