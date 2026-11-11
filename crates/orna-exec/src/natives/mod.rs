//! The engine natives: the host implementations the link step binds `core` declarations to.
//!
//! Each family is one module, copied from the old evaluator's family of the same name. The old `Value` is private to
//! the old evaluator, so these functions work on [`crate::value::Value`] and share no code with it.

mod base64;
mod bits;
mod call;
mod collection;
mod hash;
mod json;
mod order;
mod orna;
mod ovb;
#[cfg(test)]
mod registry_test;
#[cfg(test)]
mod registry_tests;
mod schema;
mod stats;
mod text;
mod time;
mod time_format;
mod unicode_16_case_properties;
mod unicode_case;
mod values;

use orna_hir::{DefId, NativeKey, Ty};

use crate::error::Stop;
use crate::link;
use crate::machine::Machine;
use crate::value::Value;

/// One engine native. It runs on the machine, which holds the limits and the step counter it charges.
pub(crate) type NativeFn = fn(&mut Machine<'_>, &[Ty], Vec<Value>) -> Result<Value, Stop>;

/// The engine native implementing the declaration `key`, if the registry has one.
pub(crate) fn lookup(key: &NativeKey) -> Option<NativeFn> {
    let module = key.module.to_string();
    Some(match (module.as_str(), key.name.as_str()) {
        ("core.encoding.base64", "encode") => base64::encode,
        ("core.encoding.base64", "decode") => base64::decode,
        ("core.encoding.json", "encode") => json::encode,
        ("core.encoding.orna", "encode") => orna::encode,
        ("core.encoding.orna", "decode") => orna::decode,
        ("core.encoding.json", "decode") => json::decode,
        ("core.encoding.ovb", "encode") => ovb::encode,
        ("core.encoding.ovb", "decode") => ovb::decode,
        ("core.encoding.json", "decode_with_options") => json::decode_with_options,
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
        ("core.collection", "count") | ("core.collection", "__list_length") => {
            collection::basic::count
        }
        ("core.collection", "union") | ("core.collection", "__list_concat") => {
            collection::basic::union_rows
        }
        ("core.collection", "sum") | ("core.collection", "__numeric_sum") => collection::basic::sum,
        ("core.collection", "first") => collection::basic::first,
        ("core.collection", "last") => collection::basic::last,
        ("core.collection", "min") | ("core.collection", "__minimum") => {
            collection::basic::min_value
        }
        ("core.collection", "max") | ("core.collection", "__maximum") => {
            collection::basic::max_value
        }
        ("core.collection", "take") => collection::basic::take_rows,
        ("core.collection", "drop") => collection::basic::drop_rows,
        ("core.collection", "distinct") => collection::basic::distinct,
        ("core.collection", "map") => collection::closure::map,
        ("core.collection", "flat_map") => collection::closure::flat_map,
        ("core.collection", "filter") => collection::closure::filter,
        ("core.collection", "every") => collection::closure::every,
        ("core.collection", "exists") => collection::closure::exists,
        ("core.collection", "one") => collection::closure::one,
        ("core.collection", "sort_by") | ("core.collection", "__stable_sort") => {
            collection::closure::sort_by
        }
        ("core.collection", "rank") => collection::closure::rank,
        ("core.collection", "group_by") => collection::keyed::group_by,
        ("core.collection", "asof_join") => collection::keyed::asof_join,
        ("core.stats", "sum") => collection::basic::sum,
        ("core.stats", "min") => stats::min,
        ("core.stats", "max") => stats::max,
        ("core.stats", "range") => stats::range,
        ("core.stats", "mode") => stats::mode,
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
        ("core.time", "offset_at") => time::offset_at,
        ("core.time", "resolve_local") => time::resolve_local,
        ("core.time", "duration_compact_format") => time_format::duration_compact_format,
        ("core.time", "duration_clock_format") => time_format::duration_clock_format,
        ("core.time", "duration_words_format") => time_format::duration_words_format,
        ("core.time", "duration_iso_format") => time_format::duration_iso_format,
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
    /// Runs the engine native bound to `def` on `arguments`, which are in parameter order. `targs` are the call's
    /// generic arguments. A declaration the registry does not implement stops the run as not yet ported.
    pub(crate) fn native(
        &mut self,
        def: DefId,
        targs: &[Ty],
        arguments: Vec<Value>,
    ) -> Result<Value, Stop> {
        let natives = self.natives.get_or_init(|| link::bind(self.program));
        let Some(native) = natives.get(&def).copied() else {
            return Err(Stop::NotYet("engine native not ported yet (H5)"));
        };
        native(self, targs, arguments)
    }
}
