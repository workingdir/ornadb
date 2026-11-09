//! Shared test support: the pre-reform binding table, copied unchanged.
//!
//! This module is compiled into the integration tests that need the table. It
//! exists for stage S1 only; stage S2 deletes the table and these tests with it.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use orna_intrinsics::{Intrinsic, IntrinsicId, Module, REEXPORTS, catalogue, lookup};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandardBindingKind {
    Collection,
    Math,
    Text,
    Bits,
    Stream,
    Concurrent,
    Ui,
    Stats,
    Time,
    Hash,
    Random,
    Base64,
    Json,
    OrnaCodec,
    OvbCodec,
    Money,
}

#[derive(Clone, Debug)]
pub struct StandardBindingModule {
    pub prefix: &'static str,
    pub kind: StandardBindingKind,
    pub operations: Vec<&'static str>,
}

fn module(
    prefix: &'static str,
    kind: StandardBindingKind,
    operations: &[&'static str],
) -> StandardBindingModule {
    StandardBindingModule {
        prefix,
        kind,
        operations: operations.to_vec(),
    }
}

/// `STANDARD_BINDING_MODULES` from `crates/orna-evaluator/src/lib.rs`.
pub fn table() -> Vec<StandardBindingModule> {
    vec![
        module(
            "std.collection.",
            StandardBindingKind::Collection,
            &[
                "chunk",
                "flatten",
                "filter",
                "map",
                "flat_map",
                "sort_by",
                "rank",
                "take",
                "drop",
                "distinct",
                "unique",
                "union",
                "count",
                "first",
                "one",
                "sum",
                "min",
                "max",
                "every",
                "exists",
                "partition",
                "zip",
                "zip_exact",
                "group_by",
                "pairs",
                "window",
                "split_when",
                "bucket_by",
                "asof_join",
                "__list_length",
                "__list_concat",
                "__numeric_sum",
                "__stable_sort",
                "__minimum",
                "__maximum",
                "__group_by",
                "__rank",
                "__asof_join",
                "__bucket_by",
            ],
        ),
        module(
            "std.query.",
            StandardBindingKind::Collection,
            &[
                "chunk",
                "flatten",
                "filter",
                "map",
                "flat_map",
                "sort_by",
                "rank",
                "take",
                "drop",
                "distinct",
                "recursive_cte",
                "__recursive_cte",
                "unique",
                "union",
                "count",
                "first",
                "one",
                "sum",
                "min",
                "max",
                "every",
                "exists",
                "partition",
                "zip",
                "zip_exact",
                "group_by",
                "pairs",
                "window",
                "split_when",
                "bucket_by",
                "asof_join",
                "__list_length",
                "__list_concat",
                "__numeric_sum",
                "__stable_sort",
                "__minimum",
                "__maximum",
                "__group_by",
                "__rank",
                "__asof_join",
                "__bucket_by",
                "__strictly_ordered_time_series",
                "__window_rate",
                "__window_rate_integrate",
                "__window_derivative",
                "__window_integrate",
            ],
        ),
        module("std.math.", StandardBindingKind::Math, &[]),
        module(
            "std.text.",
            StandardBindingKind::Text,
            &[
                "trim",
                "split",
                "join",
                "starts_with",
                "ends_with",
                "contains",
                "replace",
                "normalise",
                "lower",
                "upper",
                "__trim",
                "__split",
                "__join",
                "__starts_with",
                "__ends_with",
                "__contains",
                "__replace",
                "__normalise",
                "__lower",
                "__upper",
            ],
        ),
        module(
            "std.bits.",
            StandardBindingKind::Bits,
            &[
                "bit_or",
                "bit_and",
                "bit_xor",
                "bit_not",
                "shift_left",
                "shift_right",
                "__bit_or",
                "__bit_and",
                "__bit_xor",
                "__bit_not",
                "__shift_left",
                "__shift_right",
            ],
        ),
        module(
            "std.stream.",
            StandardBindingKind::Stream,
            &[
                "from_provider",
                "from_list",
                "for_each",
                "batch",
                "buffer",
                "merge",
                "throttle",
                "debounce",
                "retry",
                "recover",
            ],
        ),
        module(
            "std.concurrent.",
            StandardBindingKind::Concurrent,
            &["parallel", "race", "timeout"],
        ),
        module("std.ui.", StandardBindingKind::Ui, &["__node"]),
        module(
            "std.stats.",
            StandardBindingKind::Stats,
            &[
                "__mean",
                "__median",
                "__percentile",
                "__sum",
                "__min",
                "__max",
                "__range",
                "__mode",
                "__variance",
                "__standard_deviation",
                "__histogram",
                "__rate",
                "__derivative",
                "__integrate",
            ],
        ),
        module(
            "std.time.",
            StandardBindingKind::Time,
            &[
                "offset_at",
                "resolve_local",
                "duration.compact.format",
                "duration.clock.format",
                "duration.words.format",
                "duration.iso.format",
            ],
        ),
        module(
            "std.hash.",
            StandardBindingKind::Hash,
            &[
                "sha256",
                "sha256_text",
                "domain_sha256",
                "to_hex",
                "from_hex",
            ],
        ),
        module(
            "std.random.",
            StandardBindingKind::Random,
            &[
                "bytes",
                "integer",
                "choose",
                "shuffle",
                "__bytes",
                "__integer",
                "__choose",
                "__shuffle",
            ],
        ),
        module(
            "std.encoding.base64.",
            StandardBindingKind::Base64,
            &["encode", "decode", "__encode", "__decode"],
        ),
        module(
            "std.encoding.json.",
            StandardBindingKind::Json,
            &["__encode", "__decode", "__decode_with_options"],
        ),
        module(
            "std.encoding.orna.",
            StandardBindingKind::OrnaCodec,
            &["__encode", "__decode"],
        ),
        module(
            "std.encoding.ovb.",
            StandardBindingKind::OvbCodec,
            &["__encode", "__decode"],
        ),
        module(
            "std.money.",
            StandardBindingKind::Money,
            &["__quantize", "__allocate", "__format"],
        ),
    ]
}

/// Every `(prefix, operation)` the table registers.
pub fn registered_spellings() -> Vec<(&'static str, &'static str)> {
    table()
        .into_iter()
        .flat_map(|binding| {
            binding
                .operations
                .into_iter()
                .map(move |operation| (binding.prefix, operation))
        })
        .collect()
}

pub fn module_of_prefix(prefix: &str) -> Option<Module> {
    Some(match prefix {
        "std.collection." => Module::Collection,
        "std.query." => Module::Query,
        "std.math." => Module::Math,
        "std.text." => Module::Text,
        "std.bits." => Module::Bits,
        "std.stream." => Module::Stream,
        "std.concurrent." => Module::Concurrent,
        "std.ui." => Module::Ui,
        "std.stats." => Module::Stats,
        "std.time." => Module::Time,
        "std.hash." => Module::Hash,
        "std.random." => Module::Random,
        "std.encoding.base64." => Module::Base64,
        "std.encoding.json." => Module::Json,
        "std.encoding.orna." => Module::OrnaCodec,
        "std.encoding.ovb." => Module::OvbCodec,
        "std.money." => Module::Money,
        _ => return None,
    })
}

pub fn kind_matches(module: Module, kind: StandardBindingKind) -> bool {
    matches!(
        (module, kind),
        (
            Module::Collection | Module::Query,
            StandardBindingKind::Collection
        ) | (Module::Math, StandardBindingKind::Math)
            | (Module::Text, StandardBindingKind::Text)
            | (Module::Bits, StandardBindingKind::Bits)
            | (Module::Stream, StandardBindingKind::Stream)
            | (Module::Concurrent, StandardBindingKind::Concurrent)
            | (Module::Ui, StandardBindingKind::Ui)
            | (Module::Stats, StandardBindingKind::Stats)
            | (Module::Time, StandardBindingKind::Time)
            | (Module::Hash, StandardBindingKind::Hash)
            | (Module::Random, StandardBindingKind::Random)
            | (Module::Base64, StandardBindingKind::Base64)
            | (Module::Json, StandardBindingKind::Json)
            | (Module::OrnaCodec, StandardBindingKind::OrnaCodec)
            | (Module::OvbCodec, StandardBindingKind::OvbCodec)
            | (Module::Money, StandardBindingKind::Money)
    )
}

/// Descriptors the table cannot reach but the inventory classifies.
pub const INVENTORY_ONLY: &[IntrinsicId] = &[
    IntrinsicId::CoreLast,
    IntrinsicId::MathIncrement,
    IntrinsicId::MathDecrement,
    IntrinsicId::MathIsZero,
    IntrinsicId::MathMin,
    IntrinsicId::MathMax,
    IntrinsicId::MathClamp,
];

/// Public names of descriptors whose table spelling is the private leaf.
pub const CANONICAL_ALIASES: &[(Module, &str)] = &[
    (Module::Stats, "mean"),
    (Module::Stats, "median"),
    (Module::Stats, "percentile"),
    (Module::Stats, "sum"),
    (Module::Stats, "min"),
    (Module::Stats, "max"),
    (Module::Stats, "range"),
    (Module::Stats, "mode"),
    (Module::Stats, "variance"),
    (Module::Stats, "standard_deviation"),
    (Module::Stats, "histogram"),
    (Module::Stats, "rate"),
    (Module::Stats, "derivative"),
    (Module::Stats, "integrate"),
    (Module::Ui, "node"),
    (Module::Query, "window_rate"),
    (Module::Query, "window_rate_integrate"),
    (Module::Query, "window_derivative"),
    (Module::Query, "window_integrate"),
    (Module::Json, "encode"),
    (Module::Json, "decode"),
    (Module::Json, "decode_with_options"),
    (Module::OrnaCodec, "encode"),
    (Module::OrnaCodec, "decode"),
    (Module::OvbCodec, "encode"),
    (Module::OvbCodec, "decode"),
    (Module::Money, "quantize"),
    (Module::Money, "allocate"),
    (Module::Money, "format"),
];

pub fn spellings_of(intrinsic: &Intrinsic) -> Vec<(Module, &'static str)> {
    let mut spellings = vec![(intrinsic.module, intrinsic.name)];
    if let Some(leaf) = intrinsic.leaf {
        spellings.push((intrinsic.module, leaf));
    }
    for (module, name, id) in REEXPORTS {
        if *id == intrinsic.id {
            spellings.push((*module, *name));
        }
    }
    spellings
}

/// Audits the catalogue against one table. Returns the first drift found.
pub fn audit(table: &[StandardBindingModule]) -> Result<(), String> {
    let mut registered: BTreeMap<(Module, &'static str), ()> = BTreeMap::new();
    for binding in table {
        let module = module_of_prefix(binding.prefix)
            .ok_or_else(|| format!("unknown table prefix {}", binding.prefix))?;
        if !kind_matches(module, binding.kind) {
            return Err(format!("{} has a mismatched binding kind", binding.prefix));
        }
        for operation in &binding.operations {
            if registered.insert((module, *operation), ()).is_some() {
                return Err(format!("{} registers {operation} twice", binding.prefix));
            }
        }
    }

    let mut reached = BTreeSet::new();
    for (module, name) in registered.keys() {
        let intrinsic = lookup(*module, name).ok_or_else(|| {
            format!(
                "registered spelling {}.{name} has no descriptor",
                module.name()
            )
        })?;
        if !spellings_of(intrinsic).contains(&(*module, *name)) {
            return Err(format!(
                "registered spelling {}.{name} resolves to {} in the wrong module",
                module.name(),
                intrinsic.id.name()
            ));
        }
        reached.insert(intrinsic.id);
    }

    for intrinsic in catalogue() {
        if intrinsic.module == Module::Core {
            if reached.contains(&intrinsic.id) {
                return Err(format!(
                    "root intrinsic {} must not be reached from the std table",
                    intrinsic.id.name()
                ));
            }
            continue;
        }
        if INVENTORY_ONLY.contains(&intrinsic.id) {
            if reached.contains(&intrinsic.id) {
                return Err(format!(
                    "inventory-only {} must not be reached from the std table",
                    intrinsic.id.name()
                ));
            }
            continue;
        }
        if !reached.contains(&intrinsic.id) {
            return Err(format!(
                "descriptor {} has no registered spelling",
                intrinsic.id.name()
            ));
        }
        for spelling in spellings_of(intrinsic) {
            if !registered.contains_key(&spelling) && !CANONICAL_ALIASES.contains(&spelling) {
                return Err(format!(
                    "descriptor {} claims undocumented spelling {}.{}",
                    intrinsic.id.name(),
                    spelling.0.name(),
                    spelling.1
                ));
            }
        }
    }

    let expected = catalogue()
        .iter()
        .filter(|intrinsic| intrinsic.module != Module::Core)
        .filter(|intrinsic| !INVENTORY_ONLY.contains(&intrinsic.id))
        .count();
    if reached.len() != expected {
        return Err(format!(
            "reached {} descriptors, expected {expected}",
            reached.len()
        ));
    }
    Ok(())
}
