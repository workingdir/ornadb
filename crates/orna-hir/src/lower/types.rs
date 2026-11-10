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

use orna_syntax::TypeExpr;

use crate::{DefId, EffectRow, FnTy, ModuleId, Name, NameTarget, ParamIdx, Ty, TyKind};

use super::declare::Builder;

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
        "Unit" | "Nothing" => "Unit",
        "Never" => "Never",
        _ => return None,
    })
}

/// Lowers one written type in the module `module` and generics `generics`.
pub(crate) fn lower_type(
    builder: &mut Builder<'_>,
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
        // `A * B` and `A / B` are dimension products. H1 keeps the base type
        // of the left operand; the dimension algebra arrives in H2.
        TypeExpr::Product { lhs, .. } => lower_type(builder, module, generics, lhs),
    }
}

/// Lowers a named type, with or without arguments.
fn lower_named(
    builder: &mut Builder<'_>,
    module: ModuleId,
    generics: &[Name],
    path: &[String],
    arguments: &[TypeExpr],
    span: &orna_syntax::SyntaxSpan,
) -> Ty {
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
        if let Some(inner) = language_constructor(&name, &args) {
            return builder.program.tys.intern(inner);
        }
        let target = builder.resolve_bare(module, &name);
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
                    crate::diag::UNRESOLVED,
                    format!("`{name}` is not a declared type"),
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

/// Lowers a builtin scalar spelling and records its use; `None` when the spelling is not builtin.
fn lower_builtin(
    builder: &mut Builder<'_>,
    module: ModuleId,
    spelling: &str,
    span: &orna_syntax::SyntaxSpan,
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
fn nominal(builder: &mut Builder<'_>, def: DefId, args: Vec<Ty>) -> Ty {
    if builder.is_nominal(def) {
        builder
            .program
            .tys
            .intern(TyKind::Adt(def, args.into_boxed_slice()))
    } else {
        builder.program.tys.error()
    }
}
