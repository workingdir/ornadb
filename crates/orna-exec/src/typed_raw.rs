//! Canonical raw values read back as executor values at a declared type.
//!
//! `decode<T>` names the type its bytes must have. A raw value whose shape does not match that type is the value
//! error `ORNA-EVAL-VALUE`. A nominal record's omitted field takes its declared default; a field without a default
//! must be present. A type this stage does not read back is [`Stop::NotYet`], never a guess.

use std::rc::Rc;

use num_bigint::BigInt;
use orna_hir::{BodyId, DefId, DefKind, FieldInitializer, Name, Program, Ty, TyKind};
use orna_value::{OPTION_TAG, Raw, TUPLE_TAG, UNIT_TAG};

use crate::error::Stop;
use crate::frame::type_args;
use crate::machine::Machine;
use crate::raw_value;
use crate::value::Value;

const SHAPE: &str = "ORNA-EVAL-VALUE";
const UNSUPPORTED: &str = "decoded type shape (H6)";

/// A field a record must carry: its name, its declared type, and the closed body that supplies it when absent.
struct Declared {
    name: Name,
    ty: Ty,
    default: Option<BodyId>,
}

/// The executor value of the canonical raw value `raw` read at type `ty`. `env` binds the type parameters of `ty`.
pub(crate) fn from_raw_at(
    machine: &mut Machine<'_>,
    raw: &Raw,
    ty: Ty,
    env: &[Ty],
) -> Result<Value, Stop> {
    let program = machine.program;
    match program.tys.kind(ty) {
        Some(TyKind::Param(index)) => {
            let bound = env
                .get(index.as_u32() as usize)
                .copied()
                .ok_or(Stop::NotYet(
                    "type parameter outside the decode environment (H5)",
                ))?;
            from_raw_at(machine, raw, bound, &[])
        }
        Some(TyKind::Bool) => match raw {
            Raw::Bool(value) => Ok(Value::Bool(*value)),
            _ => Err(Stop::Error(SHAPE)),
        },
        Some(TyKind::Int) => match raw {
            Raw::Int(value) => Ok(Value::Int(value.clone())),
            _ => Err(Stop::Error(SHAPE)),
        },
        Some(TyKind::Str) => match raw {
            Raw::Text(value) => Ok(Value::Text(value.clone())),
            _ => Err(Stop::Error(SHAPE)),
        },
        Some(TyKind::Blob) => match raw {
            Raw::Bytes(bytes) => Ok(Value::Blob(Rc::from(bytes.as_slice()))),
            _ => Err(Stop::Error(SHAPE)),
        },
        Some(TyKind::Unit) => unit_at(raw),
        Some(TyKind::Option(item)) => option_at(machine, raw, *item, env),
        Some(TyKind::List(item)) => list_at(machine, raw, *item, env),
        Some(TyKind::Tuple(items)) => tuple_at(machine, raw, items, env),
        Some(TyKind::Record(fields)) => {
            let declared: Vec<Declared> = fields
                .iter()
                .map(|(name, ty)| Declared {
                    name: name.clone(),
                    ty: *ty,
                    default: None,
                })
                .collect();
            let Raw::Map(entries) = raw else {
                return Err(Stop::Error(SHAPE));
            };
            record_at(machine, entries, &declared, env)
        }
        Some(TyKind::Adt(def, args)) if matches!(program.defs[*def].kind, DefKind::Type(_)) => {
            nominal_at(machine, raw, *def, args, env)
        }
        _ => Err(Stop::NotYet(UNSUPPORTED)),
    }
}

/// The unit value: the canonical unit tag over an empty array.
fn unit_at(raw: &Raw) -> Result<Value, Stop> {
    match raw {
        Raw::Tag(UNIT_TAG, body) if is_empty_array(body) => Ok(Value::Unit),
        _ => Err(Stop::Error(SHAPE)),
    }
}

fn is_empty_array(body: &Raw) -> bool {
    matches!(body, Raw::Array(items) if items.is_empty())
}

/// An optional value: `[0]` for none, `[1, value]` for some.
fn option_at(machine: &mut Machine<'_>, raw: &Raw, item: Ty, env: &[Ty]) -> Result<Value, Stop> {
    let Raw::Tag(OPTION_TAG, body) = raw else {
        return Err(Stop::Error(SHAPE));
    };
    let Raw::Array(parts) = &**body else {
        return Err(Stop::Error(SHAPE));
    };
    match parts.as_slice() {
        [Raw::Int(tag)] if *tag == BigInt::from(0) => Ok(Value::Option(None)),
        [Raw::Int(tag), payload] if *tag == BigInt::from(1) => Ok(Value::Option(Some(Rc::new(
            from_raw_at(machine, payload, item, env)?,
        )))),
        _ => Err(Stop::Error(SHAPE)),
    }
}

fn list_at(machine: &mut Machine<'_>, raw: &Raw, item: Ty, env: &[Ty]) -> Result<Value, Stop> {
    let Raw::Array(items) = raw else {
        return Err(Stop::Error(SHAPE));
    };
    let values = items
        .iter()
        .map(|raw| from_raw_at(machine, raw, item, env))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::List(values.into()))
}

/// A tuple is its elements under the tuple tag. The empty tuple shares the unit encoding.
fn tuple_at(machine: &mut Machine<'_>, raw: &Raw, types: &[Ty], env: &[Ty]) -> Result<Value, Stop> {
    if types.is_empty() {
        return unit_at(raw);
    }
    let Raw::Tag(TUPLE_TAG, body) = raw else {
        return Err(Stop::Error(SHAPE));
    };
    let Raw::Array(items) = &**body else {
        return Err(Stop::Error(SHAPE));
    };
    if items.len() != types.len() {
        return Err(Stop::Error(SHAPE));
    }
    let values = items
        .iter()
        .zip(types)
        .map(|(raw, ty)| from_raw_at(machine, raw, *ty, env))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Value::Tuple(values.into()))
}

/// A nominal record of declared type `def`, instantiated at the type arguments `args`.
fn nominal_at(
    machine: &mut Machine<'_>,
    raw: &Raw,
    def: DefId,
    args: &[Ty],
    env: &[Ty],
) -> Result<Value, Stop> {
    let program = machine.program;
    let Raw::Map(entries) = raw else {
        return Err(Stop::Error(SHAPE));
    };
    let DefKind::Type(type_def) = &program.defs[def].kind else {
        return Err(Stop::NotYet(UNSUPPORTED));
    };
    let declared: Vec<Declared> = type_def
        .members
        .iter()
        .filter_map(|member| declared_field(program, *member))
        .collect();
    let inner = machine.resolve_targs(args, env)?;
    record_at(machine, entries, &declared, type_args(&inner))
}

/// The field a nominal type's member names, if the member is a field.
fn declared_field(program: &Program, member: DefId) -> Option<Declared> {
    let def = &program.defs[member];
    let DefKind::Field(field) = &def.kind else {
        return None;
    };
    let default = match field.initializer {
        Some(FieldInitializer::Default(body)) => Some(body),
        _ => None,
    };
    Some(Declared {
        name: def.name.clone(),
        ty: field.ty,
        default,
    })
}

/// A record whose entries are exactly the `declared` fields, with omitted fields taking their defaults.
fn record_at(
    machine: &mut Machine<'_>,
    entries: &[(Raw, Raw)],
    declared: &[Declared],
    env: &[Ty],
) -> Result<Value, Stop> {
    let mut seen: Vec<&str> = Vec::with_capacity(entries.len());
    for (key, _) in entries {
        let Raw::Text(name) = key else {
            return Err(Stop::Error(SHAPE));
        };
        let known = declared
            .iter()
            .any(|field| field.name.as_str() == name.as_str());
        if !known || seen.contains(&name.as_str()) {
            return Err(Stop::Error(SHAPE));
        }
        seen.push(name.as_str());
    }
    let mut fields = Vec::with_capacity(declared.len());
    for field in declared {
        let present = entries.iter().find(
            |(key, _)| matches!(key, Raw::Text(name) if name.as_str() == field.name.as_str()),
        );
        let value = match (present, field.default) {
            (Some((_, raw)), _) => from_raw_at(machine, raw, field.ty, env)?,
            (None, Some(body)) => machine.invoke(body, Vec::new(), &[], None)?,
            (None, None) => return Err(Stop::Error(SHAPE)),
        };
        fields.push((field.name.clone(), value));
    }
    Ok(raw_value::record(fields))
}
