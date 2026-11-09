//! Exhaustive oracle comparison against the pre-reform binding table.
//!
//! The table is copied unchanged into `support`. The audit fails if a
//! registered spelling has no descriptor, if a descriptor has no registered
//! spelling, or if a descriptor claims a spelling the table does not contain
//! (except the documented canonical aliases). The mutation tests prove the
//! audit detects each kind of drift.

mod support;

use std::collections::BTreeSet;

use orna_intrinsics::{IntrinsicId, Module, by_id, lookup};
use support::{CANONICAL_ALIASES, audit, spellings_of, table};

#[test]
fn catalogue_equals_the_copied_standard_binding_modules() {
    let result = audit(&table());
    assert!(result.is_ok(), "{}", result.unwrap_err());
}

#[test]
fn removing_a_registered_spelling_is_detected() {
    let mut table = table();
    table
        .iter_mut()
        .find(|binding| binding.prefix == "std.hash.")
        .expect("std.hash table")
        .operations
        .retain(|operation| *operation != "sha256");
    assert!(audit(&table).is_err());
}

#[test]
fn adding_an_unknown_spelling_is_detected() {
    let mut table = table();
    table
        .iter_mut()
        .find(|binding| binding.prefix == "std.text.")
        .expect("std.text table")
        .operations
        .push("not_an_intrinsic");
    assert!(audit(&table).is_err());
}

#[test]
fn moving_a_spelling_to_the_wrong_module_is_detected() {
    let mut table = table();
    let hash = table
        .iter_mut()
        .find(|binding| binding.prefix == "std.hash.")
        .expect("std.hash table");
    hash.operations.retain(|operation| *operation != "sha256");
    let text = table
        .iter_mut()
        .find(|binding| binding.prefix == "std.text.")
        .expect("std.text table");
    text.operations.push("sha256");
    assert!(audit(&table).is_err());
}

#[test]
fn removing_the_last_spelling_of_a_descriptor_is_detected() {
    let mut table = table();
    table
        .iter_mut()
        .find(|binding| binding.prefix == "std.stream.")
        .expect("std.stream table")
        .operations
        .retain(|operation| *operation != "from_list");
    assert!(audit(&table).is_err());
}

#[test]
fn adding_a_duplicate_spelling_is_detected() {
    let mut table = table();
    table
        .iter_mut()
        .find(|binding| binding.prefix == "std.hash.")
        .expect("std.hash table")
        .operations
        .push("sha256");
    assert!(audit(&table).is_err());
}

#[test]
fn documented_alias_set_is_exact() {
    let aliases = CANONICAL_ALIASES
        .iter()
        .map(|(module, name)| (*module, *name))
        .collect::<BTreeSet<_>>();
    assert_eq!(aliases.len(), CANONICAL_ALIASES.len());
    assert_eq!(aliases.len(), 29);
    for (module, name) in CANONICAL_ALIASES {
        let (module, name) = (*module, *name);
        let intrinsic = lookup(module, name).expect("alias resolves");
        assert!(
            spellings_of(intrinsic).contains(&(module, name)),
            "alias {}.{name} is not a descriptor spelling",
            module.name()
        );
    }
    assert_eq!(by_id(IntrinsicId::CoreLast).name, "last");
    assert_eq!(by_id(IntrinsicId::CoreLast).module, Module::Collection);
}
