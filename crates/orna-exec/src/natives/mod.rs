//! The engine natives: the host implementations the link step binds `core` declarations to.
//!
//! Each family is one module, copied from the old evaluator's family of the same name. The old `Value` is private to
//! the old evaluator, so these functions work on [`crate::value::Value`] and share no code with it.

mod base64;
mod bits;
mod hash;
#[cfg(test)]
mod registry_test;
mod text;
mod unicode_16_case_properties;
mod unicode_case;

use orna_hir::{DefId, NativeKey};

use crate::error::Stop;
use crate::link;
use crate::machine::Machine;
use crate::value::Value;

/// One engine native. It runs on the machine, which holds the limits and the step counter it charges.
pub(crate) type NativeFn = fn(&mut Machine<'_>, Vec<Value>) -> Result<Value, Stop>;

/// The engine native implementing the declaration `key`, if the registry has one.
pub(crate) fn lookup(key: &NativeKey) -> Option<NativeFn> {
    let module = key.module.to_string();
    Some(match (module.as_str(), key.name.as_str()) {
        ("core.encoding.base64", "encode") => base64::encode,
        ("core.encoding.base64", "decode") => base64::decode,
        ("core.bits", "bit_or") => bits::bit_or,
        ("core.bits", "bit_and") => bits::bit_and,
        ("core.bits", "bit_xor") => bits::bit_xor,
        ("core.bits", "bit_not") => bits::bit_not,
        ("core.bits", "shift_left") => bits::shift_left,
        ("core.bits", "shift_right") => bits::shift_right,
        ("core.hash", "sha256") => hash::sha256,
        ("core.hash", "sha256_text") => hash::sha256_text,
        ("core.hash", "domain_sha256") => hash::domain_sha256,
        ("core.hash", "to_hex") => hash::to_hex,
        ("core.hash", "from_hex") => hash::from_hex,
        ("core.text", "trim") => text::trim,
        ("core.text", "split") => text::split,
        ("core.text", "join") => text::join,
        ("core.text", "starts_with") => text::starts_with,
        ("core.text", "ends_with") => text::ends_with,
        ("core.text", "contains") => text::contains,
        ("core.text", "replace") => text::replace,
        ("core.text", "normalise") => text::normalise,
        ("core.text", "lower") => text::lower,
        ("core.text", "upper") => text::upper,
        _ => return None,
    })
}

/// The argument values in parameter order. Each argument names the parameter slot it binds. A slot with no argument,
/// with two, or past the arity is an argument error.
pub(crate) fn in_parameter_order(
    arguments: Vec<(usize, Value)>,
    arity: usize,
) -> Result<Vec<Value>, Stop> {
    let mut slots: Vec<Option<Value>> = vec![None; arity];
    for (slot, value) in arguments {
        let entry = slots
            .get_mut(slot)
            .ok_or(Stop::Error("ORNA-EVAL-ARGUMENT"))?;
        if entry.replace(value).is_some() {
            return Err(Stop::Error("ORNA-EVAL-ARGUMENT"));
        }
    }
    slots
        .into_iter()
        .map(|slot| slot.ok_or(Stop::Error("ORNA-EVAL-ARGUMENT")))
        .collect()
}

impl Machine<'_> {
    /// Runs the engine native bound to `def` on `arguments`, which are in parameter order. A declaration the registry
    /// does not implement stops the run as not yet ported.
    pub(crate) fn native(&mut self, def: DefId, arguments: Vec<Value>) -> Result<Value, Stop> {
        let natives = self.natives.get_or_init(|| link::bind(self.program));
        let Some(native) = natives.get(&def).copied() else {
            return Err(Stop::NotYet("engine native not ported yet (H5)"));
        };
        native(self, arguments)
    }
}
