//! Schema-directed decoding: a parsed JSON document is read against the Orna type it must produce. A mismatch between
//! the document and the type is a value error; a type the codec does not handle is unsupported.

use std::rc::Rc;

use orna_hir::{BodyId, DefKind, Ty, TyKind};

use super::number;
use super::parse::{Node, invalid};
use super::scope::{Field, Scope, concrete_env, record_fields, resolve};
use crate::error::Stop;
use crate::float;
use crate::machine::Machine;
use crate::value::Value;

/// How the decoder treats an object key the record does not declare.
#[derive(Clone, Copy)]
pub(super) struct Options {
    pub(super) ignore_unknown_fields: bool,
}

/// Decodes `node` as a value of type `ty`, which is written in `scope`.
pub(super) fn decode<'a>(
    machine: &mut Machine<'_>,
    node: &Node,
    ty: Ty,
    scope: Option<&'a Scope<'a>>,
    options: Options,
) -> Result<Value, Stop> {
    let program = machine.program;
    let (ty, scope) = resolve(program, ty, scope)?;
    let kind = program
        .tys
        .kind(ty)
        .ok_or(Stop::Error("ORNA-EVAL-UNSUPPORTED"))?;
    match kind {
        TyKind::Bool => match node {
            Node::Bool(value) => Ok(Value::Bool(*value)),
            _ => Err(invalid()),
        },
        TyKind::Int => match node {
            Node::Number(token) => Ok(Value::Int(number::integer(token, &machine.limits)?)),
            _ => Err(invalid()),
        },
        TyKind::Decimal => match node {
            Node::Number(token) => Ok(Value::Decimal(number::decimal(token, &machine.limits)?)),
            _ => Err(invalid()),
        },
        TyKind::Float => match node {
            Node::Number(token) => Ok(float::canonical(number::float(token)?)),
            _ => Err(invalid()),
        },
        TyKind::Str => match node {
            Node::Text(text) if text.len() <= machine.limits.max_string_bytes => {
                Ok(Value::Text(text.clone()))
            }
            Node::Text(_) => Err(Stop::Error("ORNA-EVAL-LIMIT")),
            _ => Err(invalid()),
        },
        TyKind::Option(inner) => match node {
            Node::Null => Ok(Value::Option(None)),
            _ => Ok(Value::Option(Some(Rc::new(decode(
                machine, node, *inner, scope, options,
            )?)))),
        },
        TyKind::List(inner) => {
            let items = array(node, &machine.limits)?;
            let values = items
                .iter()
                .map(|item| decode(machine, item, *inner, scope, options))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::List(Rc::from(values)))
        }
        TyKind::Tuple(types) => {
            let items = array(node, &machine.limits)?;
            if items.len() != types.len() {
                return Err(invalid());
            }
            let values = items
                .iter()
                .zip(types.iter())
                .map(|(item, &item_ty)| decode(machine, item, item_ty, scope, options))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Value::Tuple(Rc::from(values)))
        }
        TyKind::Record(declared) => {
            let fields: Vec<Field<'_>> = declared
                .iter()
                .map(|(name, ty)| Field {
                    name,
                    ty: *ty,
                    default: None,
                })
                .collect();
            record(machine, node, &fields, None, scope, options)
        }
        TyKind::Adt(def, args) => match &program.defs[*def].kind {
            DefKind::Type(_) => {
                let fields = record_fields(program, *def)?;
                record(machine, node, &fields, Some(&args[..]), scope, options)
            }
            _ => Err(Stop::Error("ORNA-EVAL-UNSUPPORTED")),
        },
        _ => Err(Stop::Error("ORNA-EVAL-UNSUPPORTED")),
    }
}

/// The items of a JSON array, bounded by the collection limit.
fn array<'n>(node: &'n Node, limits: &crate::machine::ExecLimits) -> Result<&'n [Node], Stop> {
    match node {
        Node::Array(items) if items.len() <= limits.max_collection_items => Ok(items.as_slice()),
        Node::Array(_) => Err(Stop::Error("ORNA-EVAL-LIMIT")),
        _ => Err(invalid()),
    }
}

/// Decodes a JSON object into a record. A nominal record resolves its field types in a scope of its own arguments; an
/// anonymous record resolves them in `scope`. A missing field takes its default initializer, and a missing field with
/// none is a value error.
fn record<'a>(
    machine: &mut Machine<'_>,
    node: &Node,
    fields: &[Field<'_>],
    args: Option<&'a [Ty]>,
    scope: Option<&'a Scope<'a>>,
    options: Options,
) -> Result<Value, Stop> {
    let Node::Object(members) = node else {
        return Err(invalid());
    };
    if !options.ignore_unknown_fields
        && members.iter().any(|(key, _)| {
            !fields
                .iter()
                .any(|field| field.name.as_str() == key.as_str())
        })
    {
        return Err(invalid());
    }
    let child;
    let field_scope: Option<&Scope<'_>> = match args {
        Some(args) => {
            child = Scope {
                args,
                parent: scope,
            };
            Some(&child)
        }
        None => scope,
    };
    let mut values = Vec::with_capacity(fields.len());
    for field in fields {
        let value = match members
            .iter()
            .find(|(key, _)| key.as_str() == field.name.as_str())
        {
            Some((_, member)) => decode(machine, member, field.ty, field_scope, options)?,
            None => match field.default {
                Some(body) => default_value(machine, body, args.unwrap_or(&[]), scope)?,
                None => return Err(invalid()),
            },
        };
        values.push((field.name.clone(), value));
    }
    values.sort_by(|left, right| left.0.as_str().cmp(right.0.as_str()));
    Ok(Value::Record(Rc::from(values)))
}

/// Runs a field's default initializer in the record's concrete type environment.
fn default_value(
    machine: &mut Machine<'_>,
    body: BodyId,
    args: &[Ty],
    scope: Option<&Scope<'_>>,
) -> Result<Value, Stop> {
    let env = concrete_env(machine.program, args, scope)?;
    machine.invoke(body, Vec::new(), &[], env)
}
