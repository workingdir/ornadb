//! Protocol dispatch: a call to a protocol member, whose declaration has no body.
//!
//! A member call carries the protocol's arguments, then `Self`, then the member's own generic arguments, all in the
//! caller's terms. The executor resolves them against the caller's type environment (the concrete types its frame
//! was entered with). It then runs the engine's order for a derived type (D16), or the source instance the program
//! declares for `Self`.

use std::cell::OnceCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use orna_hir::{DefId, DefKind, FnBody, Program, Ty, TyKind};

use crate::engine_order;
use crate::error::Stop;
use crate::machine::Machine;
use crate::natives;
use crate::value::Value;
use crate::variants::{VariantIds, variant_ids};

/// The protocol each member belongs to, keyed by the member's definition.
pub(crate) type ProtocolMembers = OnceCell<HashMap<DefId, DefId>>;

impl<'p> Machine<'p> {
    /// The concrete type arguments of a call whose arguments are `targs`, written in the caller's terms and resolved
    /// against the caller's environment `env`. An empty list shares the machine's empty environment.
    pub(crate) fn resolve_targs(&self, targs: &[Ty], env: &[Ty]) -> Result<Rc<[Ty]>, Stop> {
        if targs.is_empty() {
            return Ok(Rc::clone(&self.empty_env));
        }
        let mut resolved = Vec::with_capacity(targs.len());
        for ty in targs {
            resolved.push(self.resolve_ty(*ty, env)?);
        }
        Ok(resolved.into())
    }

    fn resolve_ty(&self, ty: Ty, env: &[Ty]) -> Result<Ty, Stop> {
        if let Some(TyKind::Param(index)) = self.program.tys.kind(ty) {
            return env
                .get(index.as_u32() as usize)
                .copied()
                .ok_or(Stop::NotYet(
                    "type argument outside the caller's environment (H5)",
                ));
        }
        if self.mentions_param(ty) {
            return Err(Stop::NotYet(
                "type argument with a nested type parameter (H5)",
            ));
        }
        Ok(ty)
    }

    /// Whether `ty` names a type parameter anywhere in its structure.
    fn mentions_param(&self, ty: Ty) -> bool {
        match self.program.tys.kind(ty) {
            Some(TyKind::Param(_)) => true,
            Some(TyKind::Adt(_, args) | TyKind::Tuple(args)) => {
                args.iter().any(|arg| self.mentions_param(*arg))
            }
            Some(
                TyKind::Option(item)
                | TyKind::List(item)
                | TyKind::Set(item)
                | TyKind::Range(item)
                | TyKind::Relation(item)
                | TyKind::Stream(item)
                | TyKind::Quantity(item, _),
            ) => self.mentions_param(*item),
            Some(TyKind::Map(key, value)) => {
                self.mentions_param(*key) || self.mentions_param(*value)
            }
            Some(TyKind::Record(fields)) => {
                fields.iter().any(|(_, field)| self.mentions_param(*field))
            }
            Some(TyKind::Fn(function)) => {
                self.mentions_param(function.ret)
                    || function
                        .params
                        .iter()
                        .any(|param| self.mentions_param(*param))
            }
            _ => false,
        }
    }

    /// Runs one call of the protocol member `member`. `env` holds the call's resolved type arguments and `arguments`
    /// its evaluated arguments.
    pub(crate) fn dispatch(
        &mut self,
        member: DefId,
        env: &[Ty],
        arguments: Vec<(usize, Value)>,
    ) -> Result<Value, Stop> {
        let program = self.program;
        let protocol = self.protocol_of(member)?;
        let DefKind::Protocol(definition) = &program.defs[protocol].kind else {
            return Err(Stop::NotYet("protocol member without a protocol (H5)"));
        };
        let arity = definition.params.len();
        let Some(&self_ty) = env.get(arity) else {
            return Err(Stop::NotYet("protocol call without its Self argument (H5)"));
        };
        let name = program.defs[member].name.as_str();
        if name == "compare"
            && is_order(program, protocol)
            && orna_hir::engine_order::derives_order(program, self_ty)
        {
            let values = natives::in_parameter_order(arguments, 2)?;
            let ordering = engine_order::compare(&values[0], &values[1])?;
            return self.ordering_value(ordering);
        }
        let Some(instance) = self.find_instance(protocol, &env[..arity], self_ty, name) else {
            return Err(Stop::Error("ORNA-EVAL-NAME"));
        };
        let DefKind::Fn(function) = &program.defs[instance].kind else {
            return Err(Stop::NotYet("instance member that is not a function (H5)"));
        };
        let FnBody::Source(body) = function.body else {
            return Err(Stop::NotYet("instance member without a source body (H5)"));
        };
        let own: Rc<[Ty]> = if env.len() > arity + 1 {
            Rc::from(&env[arity + 1..])
        } else {
            Rc::clone(&self.empty_env)
        };
        self.invoke(body, arguments, &[], own)
    }

    /// The protocol that declares `member`. The map is built on first use.
    fn protocol_of(&self, member: DefId) -> Result<DefId, Stop> {
        self.protocols
            .get_or_init(|| protocol_members(self.program))
            .get(&member)
            .copied()
            .ok_or(Stop::NotYet("protocol member without a protocol (H5)"))
    }

    /// The implementation of `protocol` (with `protocol_args`) for the nominal type `self_ty` that declares a member
    /// named `name`. Only a nominal type can have a source instance.
    fn find_instance(
        &self,
        protocol: DefId,
        protocol_args: &[Ty],
        self_ty: Ty,
        name: &str,
    ) -> Option<DefId> {
        let program = self.program;
        let Some(TyKind::Adt(owner, _)) = program.tys.kind(self_ty) else {
            return None;
        };
        for &implementation in program.instances.get(owner)? {
            let DefKind::Impl(instance) = &program.defs[implementation].kind else {
                continue;
            };
            if instance.protocol != Some(protocol)
                || instance.protocol_args.as_ref() != protocol_args
            {
                continue;
            }
            if let Some(&member) = instance
                .members
                .iter()
                .find(|member| program.defs[**member].name.as_str() == name)
            {
                return Some(member);
            }
        }
        None
    }

    /// The `core.Ordering` value for `ordering`.
    fn ordering_value(&self, ordering: Ordering) -> Result<Value, Stop> {
        let ids = match self.ordering.get() {
            Some(ids) => *ids,
            None => {
                let ids = find_ordering(self.program)?;
                let _ = self.ordering.set(ids);
                ids
            }
        };
        let index = match ordering {
            Ordering::Less => 0,
            Ordering::Equal => 1,
            Ordering::Greater => 2,
        };
        Ok(Value::Enum {
            type_id: ids[index].type_id,
            variant_id: ids[index].variant_id,
            payload: None,
        })
    }
}

/// Whether `protocol` is the `core.ops` protocol `Order`.
fn is_order(program: &Program, protocol: DefId) -> bool {
    let definition = &program.defs[protocol];
    definition.name.as_str() == "Order"
        && program.modules[definition.module].path.to_string() == "core.ops"
}

/// Every protocol member, keyed to the protocol that declares it.
fn protocol_members(program: &Program) -> HashMap<DefId, DefId> {
    let mut members = HashMap::new();
    for (protocol, definition) in program.defs.iter_enumerated() {
        if let DefKind::Protocol(protocol_def) = &definition.kind {
            for &member in &protocol_def.members {
                members.insert(member, protocol);
            }
        }
    }
    members
}

/// The ids of `core.Ordering`'s variants: `[less, equal, greater]`.
fn find_ordering(program: &Program) -> Result<[VariantIds; 3], Stop> {
    let variants = variant_ids(program);
    for definition in program.defs.iter() {
        let DefKind::Enum(enum_def) = &definition.kind else {
            continue;
        };
        if definition.name.as_str() != "Ordering"
            || program.modules[definition.module].path.to_string() != "core"
        {
            continue;
        }
        let find = |wanted: &str| {
            enum_def
                .variants
                .iter()
                .find(|variant| program.defs[**variant].name.as_str() == wanted)
                .and_then(|variant| variants.get(variant).copied())
        };
        if let (Some(less), Some(equal), Some(greater)) =
            (find("less"), find("equal"), find("greater"))
        {
            return Ok([less, equal, greater]);
        }
    }
    Err(Stop::NotYet("core Ordering is not declared (H5)"))
}
