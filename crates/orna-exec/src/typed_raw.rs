//! Canonical raw values read back as executor values at a declared type.
//!
//! `decode<T>` names the type its bytes must have. A raw value whose shape does not match that type is the value
//! error `ORNA-EVAL-VALUE`. A nominal record's omitted field takes its declared default; a field without a default
//! must be present. A type this stage does not read back is [`Stop::NotYet`], never a guess.

use std::rc::Rc;

use num_bigint::BigInt;
use orna_hir::{BodyId, DefId, DefKind, FieldInitializer, Name, Program, Ty, TyKind};
use orna_value::{
    ENUM_TAG, OPTION_TAG, RECORD_TAG, REFINED_NOMINAL_TAG, Raw, TUPLE_TAG, UNIT_TAG, UUID_TAG,
};

use crate::engine_money::{currency_name, def_qualified_name, is_money_type};
use crate::error::Stop;
use crate::frame::{TypeEnv, type_args};
use crate::machine::Machine;
use crate::raw_value::{self, object_id};
use crate::value::{Value, key_order};
use crate::variants::enum_type_id_of;

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
        Some(TyKind::Adt(def, _)) if matches!(program.defs[*def].kind, DefKind::Enum(_)) => {
            enum_at(machine, raw, *def)
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
    let refined_base = refined_base_of(program, def, &type_def.members);
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

/// The base type a refined type's values are stored as: the self type of its first refinement, when the type declares
/// no fields. A refinement whose self type is the type itself names no base, so the type is nominal.
fn refined_base_of(program: &Program, def: DefId, members: &[DefId]) -> Option<Ty> {
    if members
        .iter()
        .any(|member| declared_field(program, *member).is_some())
    {
        return None;
    }
    let first = members
        .iter()
        .copied()
        .find(|member| assertion_body(program, *member).is_some())?;
    self_ty(program, first).filter(
        |base| !matches!(program.tys.kind(*base), Some(TyKind::Adt(owner, _)) if *owner == def),
    )
}

/// The ObjectId of the nominal type `def`, derived from its qualified name.
fn nominal_id(machine: &Machine<'_>, def: DefId) -> [u8; 16] {
    machine
        .host
        .nominal_type_id(&def_qualified_name(machine.program, def))
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

/// A source enum value at its declared enum type: `[type_id, variant_id, payload]` under the enum tag. The type id
/// must name `def`, the variant must be one of its variants, and the payload must be exactly that variant's fields
/// in a record, or `null` for a payload-free variant.
fn enum_at(machine: &mut Machine<'_>, raw: &Raw, def: DefId) -> Result<Value, Stop> {
    let program = machine.program;
    let Raw::Tag(ENUM_TAG, body) = raw else {
        return Err(Stop::Error(SHAPE));
    };
    let Raw::Array(parts) = &**body else {
        return Err(Stop::Error(SHAPE));
    };
    let [
        Raw::Tag(UUID_TAG, type_body),
        Raw::Tag(UUID_TAG, variant_body),
        payload,
    ] = parts.as_slice()
    else {
        return Err(Stop::Error(SHAPE));
    };
    let type_id = object_id(type_body).ok_or(Stop::Error(SHAPE))?;
    let variant_id = object_id(variant_body).ok_or(Stop::Error(SHAPE))?;
    let DefKind::Enum(enum_def) = &program.defs[def].kind else {
        return Err(Stop::NotYet(UNSUPPORTED));
    };
    if type_id != enum_type_id_of(program, def) {
        return Err(Stop::Error(SHAPE));
    }
    let variant = variant_of(machine, &enum_def.variants, variant_id)?;
    let DefKind::Variant(variant_def) = &program.defs[variant].kind else {
        return Err(Stop::NotYet(UNSUPPORTED));
    };
    let payload = match (payload, variant_def.fields.is_empty()) {
        (Raw::Null, true) => None,
        (Raw::Tag(RECORD_TAG, record), false) => {
            let entries = record_payload_entries(record)?;
            let declared: Vec<Declared> = variant_def
                .fields
                .iter()
                .filter_map(|member| declared_field(program, *member))
                .collect();
            match record_at(machine, &entries, &declared, &[])? {
                Value::Record(fields) => Some(fields),
                _ => return Err(Stop::Error(SHAPE)),
            }
        }
        _ => return Err(Stop::Error(SHAPE)),
    };
    Ok(Value::Enum {
        type_id,
        variant_id,
        payload,
    })
}

/// The entries of an enum payload record (60009), `[type_object_id_or_null, [[name, value], ...]]`, in the order
/// the payload writes them.
fn record_payload_entries(record: &Raw) -> Result<Vec<(Raw, Raw)>, Stop> {
    let Raw::Array(parts) = record else {
        return Err(Stop::Error(SHAPE));
    };
    let [type_part, Raw::Array(pairs)] = parts.as_slice() else {
        return Err(Stop::Error(SHAPE));
    };
    if !matches!(type_part, Raw::Null | Raw::Tag(UUID_TAG, _)) {
        return Err(Stop::Error(SHAPE));
    }
    pairs
        .iter()
        .map(|pair| match pair {
            Raw::Array(entry) => match entry.as_slice() {
                [key, value] => Ok((key.clone(), value.clone())),
                _ => Err(Stop::Error(SHAPE)),
            },
            _ => Err(Stop::Error(SHAPE)),
        })
        .collect()
}

/// The source variant of an enum, among `variants`, whose ObjectId is `variant_id`.
fn variant_of(
    machine: &Machine<'_>,
    variants: &[DefId],
    variant_id: [u8; 16],
) -> Result<DefId, Stop> {
    variants
        .iter()
        .copied()
        .find(|variant| {
            machine
                .variant_ids_of(*variant)
                .is_some_and(|ids| ids.variant_id == variant_id)
        })
        .ok_or(Stop::Error(SHAPE))
}

/// The canonical raw form of `value` at type `ty`, the inverse of [`from_raw_at`]. A refined value is written as a
/// `60020` wrapper over its base, and a source enum value as a `60008` wrapper. Composite values are written at
/// their declared element types. A value whose shape is not that composite's, and a type this stage does not write
/// typed, keep the untyped form of [`Value::to_raw`].
pub(crate) fn to_raw_at(
    machine: &mut Machine<'_>,
    value: &Value,
    ty: Ty,
    env: &[Ty],
) -> Result<Raw, Stop> {
    let program = machine.program;
    match program.tys.kind(ty) {
        Some(TyKind::Param(index)) => {
            let bound = env
                .get(index.as_u32() as usize)
                .copied()
                .ok_or(Stop::NotYet(
                    "type parameter outside the decode environment (H5)",
                ))?;
            to_raw_at(machine, value, bound, &[])
        }
        Some(TyKind::Option(item)) => match value {
            Value::Option(Some(payload)) => {
                let payload = to_raw_at(machine, payload, *item, env)?;
                Ok(Raw::Tag(
                    OPTION_TAG,
                    Box::new(Raw::Array(vec![Raw::Int(BigInt::from(1)), payload])),
                ))
            }
            _ => untyped(value),
        },
        Some(TyKind::List(item)) => match value {
            Value::List(items) => items
                .iter()
                .map(|element| to_raw_at(machine, element, *item, env))
                .collect::<Result<Vec<_>, _>>()
                .map(Raw::Array),
            _ => untyped(value),
        },
        Some(TyKind::Tuple(types)) if !types.is_empty() => match value {
            Value::Tuple(values) if values.len() == types.len() => {
                let items = values
                    .iter()
                    .zip(types.iter())
                    .map(|(element, ty)| to_raw_at(machine, element, *ty, env))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Raw::Tag(TUPLE_TAG, Box::new(Raw::Array(items))))
            }
            _ => untyped(value),
        },
        Some(TyKind::Record(fields)) => match value {
            Value::Record(values) => record_to_raw(machine, values, fields, env),
            _ => untyped(value),
        },
        Some(TyKind::Adt(def, _)) if is_money_type(program, *def) => untyped(value),
        Some(TyKind::Adt(def, _)) if matches!(program.defs[*def].kind, DefKind::Enum(_)) => {
            match value {
                Value::Enum { .. } => enum_to_raw(machine, value, *def),
                _ => untyped(value),
            }
        }
        Some(TyKind::Adt(def, args)) if matches!(program.defs[*def].kind, DefKind::Type(_)) => {
            type_to_raw(machine, value, *def, args, env)
        }
        _ => untyped(value),
    }
}

/// The untyped canonical form of `value`.
fn untyped(value: &Value) -> Result<Raw, Stop> {
    value.to_raw().map_err(Stop::Error)
}

/// An ObjectId as its UUID-tagged 16 bytes.
fn uuid_raw(id: [u8; 16]) -> Raw {
    Raw::Tag(UUID_TAG, Box::new(Raw::Bytes(id.to_vec())))
}

/// A user-defined type's value at its type. A refined type is written as `60020` over its base, which is the value
/// itself; a nominal record is its fields, each at its declared type.
fn type_to_raw(
    machine: &mut Machine<'_>,
    value: &Value,
    def: DefId,
    args: &[Ty],
    env: &[Ty],
) -> Result<Raw, Stop> {
    let program = machine.program;
    let DefKind::Type(type_def) = &program.defs[def].kind else {
        return untyped(value);
    };
    let inner = machine.resolve_targs(args, env)?;
    let inner_env = type_args(&inner);
    if let Some(base) = refined_base_of(program, def, &type_def.members) {
        let base_raw = to_raw_at(machine, value, base, inner_env)?;
        let wrapper = Raw::Array(vec![uuid_raw(nominal_id(machine, def)), base_raw]);
        return Ok(Raw::Tag(REFINED_NOMINAL_TAG, Box::new(wrapper)));
    }
    match value {
        Value::Record(values) => {
            let types = field_types(program, &type_def.members);
            record_to_raw(machine, values, &types, inner_env)
        }
        _ => untyped(value),
    }
}

/// A source enum value at its enum type: `[type_id, variant_id, payload]` under the enum tag, the payload being the
/// variant's fields in a record, or `null` for a payload-free variant.
fn enum_to_raw(machine: &mut Machine<'_>, value: &Value, def: DefId) -> Result<Raw, Stop> {
    let program = machine.program;
    let Value::Enum {
        type_id,
        variant_id,
        payload,
    } = value
    else {
        return untyped(value);
    };
    let DefKind::Enum(enum_def) = &program.defs[def].kind else {
        return Err(Stop::NotYet(UNSUPPORTED));
    };
    if *type_id != enum_type_id_of(program, def) {
        return Err(Stop::Error(SHAPE));
    }
    let variant = variant_of(machine, &enum_def.variants, *variant_id)?;
    let DefKind::Variant(variant_def) = &program.defs[variant].kind else {
        return Err(Stop::NotYet(UNSUPPORTED));
    };
    let payload_raw = match (payload, variant_def.fields.is_empty()) {
        (None, true) => Raw::Null,
        (Some(fields), false) => {
            let types = field_types(program, &variant_def.fields);
            let entries = record_entries_to_raw(machine, fields, &types, &[])?;
            let pairs = entries
                .into_iter()
                .map(|(name, value)| Raw::Array(vec![name, value]))
                .collect();
            Raw::Tag(
                RECORD_TAG,
                Box::new(Raw::Array(vec![Raw::Null, Raw::Array(pairs)])),
            )
        }
        _ => return Err(Stop::Error(SHAPE)),
    };
    let wrapper = Raw::Array(vec![uuid_raw(*type_id), uuid_raw(*variant_id), payload_raw]);
    Ok(Raw::Tag(ENUM_TAG, Box::new(wrapper)))
}

/// The declared fields among `members`, as their names and types.
fn field_types(program: &Program, members: &[DefId]) -> Vec<(Name, Ty)> {
    members
        .iter()
        .filter_map(|member| declared_field(program, *member))
        .map(|field| (field.name, field.ty))
        .collect()
}

/// A record's canonical map: each entry written at the type its name declares, in canonical key order. A field the
/// types do not declare is a value error.
fn record_to_raw(
    machine: &mut Machine<'_>,
    values: &[(Name, Value)],
    types: &[(Name, Ty)],
    env: &[Ty],
) -> Result<Raw, Stop> {
    record_entries_to_raw(machine, values, types, env).map(Raw::Map)
}

/// The canonical entries of a record, in canonical key order: each entry written at the type its name declares. A
/// field the types do not declare is a value error.
fn record_entries_to_raw(
    machine: &mut Machine<'_>,
    values: &[(Name, Value)],
    types: &[(Name, Ty)],
    env: &[Ty],
) -> Result<Vec<(Raw, Raw)>, Stop> {
    let names: Vec<&str> = values.iter().map(|(name, _)| name.as_str()).collect();
    let order = key_order(&names).map_err(Stop::Error)?;
    order
        .into_iter()
        .map(|index| -> Result<(Raw, Raw), Stop> {
            let (name, value) = &values[index];
            let ty = types
                .iter()
                .find(|(declared, _)| declared.as_str() == name.as_str())
                .map(|(_, ty)| *ty)
                .ok_or(Stop::Error(SHAPE))?;
            let raw = to_raw_at(machine, value, ty, env)?;
            Ok((Raw::Text(name.as_str().to_owned()), raw))
        })
        .collect()
}
