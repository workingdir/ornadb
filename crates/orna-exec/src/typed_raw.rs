//! Canonical raw values read back as executor values at a declared type.
//!
//! `decode<T>` names the type its bytes must have. A raw value whose shape does not match that type is the value
//! error `ORNA-EVAL-VALUE`. A nominal record's omitted field takes its declared default; a field without a default
//! must be present. A type this stage does not read back is [`Stop::NotYet`], never a guess.

use std::rc::Rc;

use num_bigint::BigInt;
use orna_hir::{BodyId, DefId, DefKind, FieldInitializer, Name, Program, Ty, TyKind};
use orna_value::{OPTION_TAG, REFINED_NOMINAL_TAG, Raw, TUPLE_TAG, UNIT_TAG, UUID_TAG};

use crate::engine_money::{currency_name, def_qualified_name, is_money_type};
use crate::error::Stop;
use crate::frame::{TypeEnv, type_args};
use crate::machine::Machine;
use crate::raw_value::{self, object_id};
use crate::value::Value;

const SHAPE: &str = "ORNA-EVAL-VALUE";
const UNSUPPORTED: &str = "decoded type shape (H6)";
const REFINEMENT_FAILED: &str = "ORNA-A091-008";

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
        Some(TyKind::Float) => leaf_at(raw, |value| matches!(value, Value::Float(_))),
        Some(TyKind::Decimal) => leaf_at(raw, |value| matches!(value, Value::Decimal(_))),
        Some(TyKind::Uuid) => leaf_at(raw, |value| matches!(value, Value::Uuid(_))),
        Some(TyKind::Digest) => leaf_at(raw, |value| matches!(value, Value::Digest(_))),
        Some(TyKind::Date) => leaf_at(raw, |value| matches!(value, Value::Date(_))),
        Some(TyKind::Instant) => leaf_at(raw, |value| matches!(value, Value::Instant { .. })),
        Some(TyKind::Duration) => leaf_at(raw, |value| matches!(value, Value::Duration { .. })),
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
        Some(TyKind::Adt(def, args)) if is_money_type(program, *def) => {
            money_at(machine, raw, args, env)
        }
        Some(TyKind::Adt(def, args)) if matches!(program.defs[*def].kind, DefKind::Type(_)) => {
            type_at(machine, raw, *def, args, env)
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

/// A user-defined type. A type with fields is a nominal record. A type whose members are only refinements is a refined
/// type, whose value is its base value. Either way every refinement holds of the value before it is returned.
fn type_at(
    machine: &mut Machine<'_>,
    raw: &Raw,
    def: DefId,
    args: &[Ty],
    env: &[Ty],
) -> Result<Value, Stop> {
    let program = machine.program;
    let DefKind::Type(type_def) = &program.defs[def].kind else {
        return Err(Stop::NotYet(UNSUPPORTED));
    };
    let declared: Vec<Declared> = type_def
        .members
        .iter()
        .filter_map(|member| declared_field(program, *member))
        .collect();
    let assertions: Vec<DefId> = type_def
        .members
        .iter()
        .copied()
        .filter(|member| assertion_body(program, *member).is_some())
        .collect();
    let inner = machine.resolve_targs(args, env)?;
    let refined_base = match assertions.first() {
        Some(first) if declared.is_empty() => self_ty(program, *first).filter(
            |base| !matches!(program.tys.kind(*base), Some(TyKind::Adt(owner, _)) if *owner == def),
        ),
        _ => None,
    };
    let value = match refined_base {
        Some(base) => {
            let base_raw = refined_base_raw(machine, raw, def)?;
            from_raw_at(machine, base_raw, base, type_args(&inner))?
        }
        None => {
            let Raw::Map(entries) = raw else {
                return Err(Stop::Error(SHAPE));
            };
            record_at(machine, entries, &declared, type_args(&inner))?
        }
    };
    check_assertions(machine, &assertions, &value, inner)?;
    Ok(value)
}

/// The base value a refined type's raw value carries. A `60020` wrapper must name this type; the executor's own
/// encoding writes the bare base, which is accepted as it is.
fn refined_base_raw<'r>(machine: &Machine<'_>, raw: &'r Raw, def: DefId) -> Result<&'r Raw, Stop> {
    let Raw::Tag(REFINED_NOMINAL_TAG, payload) = raw else {
        return Ok(raw);
    };
    let Raw::Array(parts) = &**payload else {
        return Err(Stop::Error(SHAPE));
    };
    let [Raw::Tag(UUID_TAG, id), base] = parts.as_slice() else {
        return Err(Stop::Error(SHAPE));
    };
    let expected = machine
        .host
        .nominal_type_id(&def_qualified_name(machine.program, def));
    if object_id(id) == Some(expected) {
        Ok(base)
    } else {
        Err(Stop::Error(SHAPE))
    }
}

/// A Money value, whose currency must be the currency type `Money` names.
fn money_at(machine: &mut Machine<'_>, raw: &Raw, args: &[Ty], env: &[Ty]) -> Result<Value, Stop> {
    let program = machine.program;
    let inner = machine.resolve_targs(args, env)?;
    let [currency] = type_args(&inner) else {
        return Err(Stop::NotYet(UNSUPPORTED));
    };
    let name = currency_name(program, *currency).ok_or(Stop::NotYet(UNSUPPORTED))?;
    let expected = machine.host.nominal_type_id(&name);
    let value = leaf_at(raw, |value| matches!(value, Value::Money { .. }))?;
    let Value::Money {
        currency: actual, ..
    } = &value
    else {
        return Err(Stop::Error(SHAPE));
    };
    if *actual != expected {
        return Err(Stop::Error(SHAPE));
    }
    Ok(value)
}

/// A leaf value: the canonical raw value read back by [`raw_value::from_raw`] when it has the leaf type's shape. A raw
/// value that reads back as another shape, or not at all, is not a value of the leaf type.
fn leaf_at(raw: &Raw, accepts: fn(&Value) -> bool) -> Result<Value, Stop> {
    match raw_value::from_raw(raw) {
        Ok(value) if accepts(&value) => Ok(value),
        Ok(_) | Err(Stop::NotYet(_)) => Err(Stop::Error(SHAPE)),
        Err(error) => Err(error),
    }
}

/// Each refinement of `value`, in source order. The first one that is false fails the decode.
fn check_assertions(
    machine: &mut Machine<'_>,
    assertions: &[DefId],
    value: &Value,
    env: TypeEnv,
) -> Result<(), Stop> {
    let program = machine.program;
    for member in assertions {
        let Some(body) = assertion_body(program, *member) else {
            continue;
        };
        match machine.invoke(body, vec![(0, value.clone())], &[], env.clone())? {
            Value::Bool(true) => {}
            Value::Bool(false) => return Err(Stop::Error(REFINEMENT_FAILED)),
            _ => return Err(Stop::Error("ORNA-EVAL-TYPE")),
        }
    }
    Ok(())
}

/// The body of an assertion definition, or `None` for any other definition.
fn assertion_body(program: &Program, member: DefId) -> Option<BodyId> {
    match &program.defs[member].kind {
        DefKind::Assertion(assertion) => Some(assertion.body),
        _ => None,
    }
}

/// The type an assertion's `self` names: the base value of a refined type, or the nominal row type.
fn self_ty(program: &Program, member: DefId) -> Option<Ty> {
    let body = &program.bodies[assertion_body(program, member)?];
    let slot = body.params.first()?;
    Some(body.locals[*slot].ty)
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
