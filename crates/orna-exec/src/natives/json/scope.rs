//! Resolves the types a JSON codec walks. A generic record's field types name its parameters, and a parameter stands for
//! an argument of the record type that holds the field. A scope keeps those arguments by reference, so no type is
//! interned while a value is walked.

use std::rc::Rc;

use orna_hir::{BodyId, DefId, DefKind, Name, Program, Ty, TyKind};

use crate::error::Stop;
use crate::frame::TypeEnv;

/// The type arguments of one generic record and the scope those arguments were written in.
pub(super) struct Scope<'a> {
    pub(super) args: &'a [Ty],
    pub(super) parent: Option<&'a Scope<'a>>,
}

/// One record field the codec reads or writes: its name, its type in the record's scope, and the body that supplies it
/// when the input omits it.
pub(super) struct Field<'a> {
    pub(super) name: &'a Name,
    pub(super) ty: Ty,
    pub(super) default: Option<BodyId>,
}

/// The type `ty` denotes in `scope`: a parameter is replaced by its argument, which is itself resolved in the scope
/// that holds it. A parameter with no scope to resolve it in is unsupported.
pub(super) fn resolve<'a>(
    program: &Program,
    ty: Ty,
    scope: Option<&'a Scope<'a>>,
) -> Result<(Ty, Option<&'a Scope<'a>>), Stop> {
    let mut ty = ty;
    let mut scope = scope;
    while let Some(&TyKind::Param(index)) = program.tys.kind(ty) {
        let current = scope.ok_or(Stop::Error("ORNA-EVAL-UNSUPPORTED"))?;
        ty = *current
            .args
            .get(index.as_u32() as usize)
            .ok_or(Stop::Error("ORNA-EVAL-UNSUPPORTED"))?;
        scope = current.parent;
    }
    Ok((ty, scope))
}

/// The fields of a nominal record, in declaration order. A computed field is derived from the row and never stored, so
/// it is not part of the record's JSON form. Any other kind of definition is not a record.
pub(super) fn record_fields(program: &Program, def: DefId) -> Result<Vec<Field<'_>>, Stop> {
    let DefKind::Type(type_def) = &program.defs[def].kind else {
        return Err(Stop::Error("ORNA-EVAL-UNSUPPORTED"));
    };
    let mut fields = Vec::with_capacity(type_def.members.len());
    for &member in &type_def.members {
        let definition = &program.defs[member];
        let DefKind::Field(field) = &definition.kind else {
            continue;
        };
        match field.initializer {
            Some(orna_hir::FieldInitializer::Computed(_)) => {}
            initializer => fields.push(Field {
                name: &definition.name,
                ty: field.ty,
                default: match initializer {
                    Some(orna_hir::FieldInitializer::Default(body)) => Some(body),
                    _ => None,
                },
            }),
        }
    }
    Ok(fields)
}

/// The type environment a default initializer of a generic record runs in: the record's arguments, each made concrete.
/// An argument that still mentions a parameter cannot be named without interning a new type, so it is unsupported.
pub(super) fn concrete_env(
    program: &Program,
    args: &[Ty],
    scope: Option<&Scope<'_>>,
) -> Result<TypeEnv, Stop> {
    if args.is_empty() {
        return Ok(None);
    }
    let mut concrete = Vec::with_capacity(args.len());
    for &arg in args {
        let (ty, _) = resolve(program, arg, scope)?;
        if mentions_param(program, ty) {
            return Err(Stop::Error("ORNA-EVAL-UNSUPPORTED"));
        }
        concrete.push(ty);
    }
    Ok(Some(Rc::from(concrete)))
}

/// Whether a type names a generic parameter anywhere inside it.
fn mentions_param(program: &Program, ty: Ty) -> bool {
    match program.tys.kind(ty) {
        Some(TyKind::Param(_)) => true,
        Some(
            TyKind::Option(inner)
            | TyKind::List(inner)
            | TyKind::Set(inner)
            | TyKind::Range(inner)
            | TyKind::Relation(inner)
            | TyKind::Stream(inner)
            | TyKind::Quantity(inner, _),
        ) => mentions_param(program, *inner),
        Some(TyKind::Map(key, value)) => {
            mentions_param(program, *key) || mentions_param(program, *value)
        }
        Some(TyKind::Tuple(items)) => items.iter().any(|&item| mentions_param(program, item)),
        Some(TyKind::Record(fields)) => fields
            .iter()
            .any(|(_, field)| mentions_param(program, *field)),
        Some(TyKind::Adt(_, args)) => args.iter().any(|&arg| mentions_param(program, arg)),
        _ => false,
    }
}
