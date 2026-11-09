//! Structural invariants of the intrinsic catalogue.

use std::collections::{BTreeMap, BTreeSet};

use orna_intrinsics::{
    CURRENT_STD_PIN, Intrinsic, IntrinsicId, Module, PinRange, REEXPORTS, ROOT_NAMES, SigType,
    by_id, catalogue, lookup, parameters, root_intrinsic,
};

#[test]
fn ids_and_catalogue_agree_in_declaration_order() {
    let catalogue = catalogue();
    assert_eq!(catalogue.len(), IntrinsicId::ALL.len());
    assert_eq!(catalogue.len(), 124);

    let mut seen = BTreeSet::new();
    for (index, id) in IntrinsicId::ALL.iter().copied().enumerate() {
        let intrinsic = &catalogue[index];
        assert_eq!(intrinsic.id, id, "catalogue[{index}] is not {id:?}");
        assert_eq!(usize::from(id.as_u16()), index, "id discriminant drifted");
        assert!(seen.insert(id), "{id:?} appears more than once");
    }
    assert_eq!(seen.len(), catalogue.len());
}

#[test]
fn by_id_is_total_and_parameters_are_the_declared_slice() {
    for intrinsic in catalogue() {
        let resolved = by_id(intrinsic.id);
        assert_eq!(resolved.id, intrinsic.id);
        assert_eq!(resolved.name, intrinsic.name);
        assert_eq!(parameters(intrinsic.id), intrinsic.signature.params);
    }
}

#[test]
fn every_signature_is_well_formed() {
    for intrinsic in catalogue() {
        assert!(
            intrinsic.signature.is_well_formed(),
            "{} has an unbound type parameter",
            intrinsic.id.name()
        );
        assert!(
            !intrinsic.note.is_empty(),
            "{} has no note",
            intrinsic.id.name()
        );
        assert!(
            intrinsic.note.is_ascii(),
            "{} has a non-ASCII note",
            intrinsic.id.name()
        );
    }
}

#[test]
fn lookup_agrees_with_every_descriptor_spelling() {
    for intrinsic in catalogue() {
        let by_name = lookup(intrinsic.module, intrinsic.name)
            .unwrap_or_else(|| panic!("{} name is not registered", intrinsic.id.name()));
        assert_eq!(by_name.id, intrinsic.id);
        if let Some(leaf) = intrinsic.leaf {
            let by_leaf = lookup(intrinsic.module, leaf)
                .unwrap_or_else(|| panic!("{} leaf is not registered", intrinsic.id.name()));
            assert_eq!(by_leaf.id, intrinsic.id);
        }
    }
}

#[test]
fn registered_spellings_are_unique_across_descriptors() {
    let mut owners: BTreeMap<(Module, &str), IntrinsicId> = BTreeMap::new();
    for intrinsic in catalogue() {
        for spelling in std::iter::once(intrinsic.name).chain(intrinsic.leaf) {
            let previous = owners.insert((intrinsic.module, spelling), intrinsic.id);
            assert!(
                previous.is_none(),
                "{} is claimed by both {previous:?} and {}",
                spelling,
                intrinsic.id.name()
            );
        }
    }

    let mut reexport_owners: BTreeMap<(Module, &str), IntrinsicId> = BTreeMap::new();
    for (module, name, id) in REEXPORTS {
        let previous = reexport_owners.insert((*module, *name), *id);
        assert!(
            previous.is_none(),
            "re-export {}.{name} is duplicated",
            module.name()
        );
        assert_eq!(by_id(*id).module, Module::Collection);
    }
}

#[test]
fn root_environment_is_exactly_the_core_ids() {
    for (name, id) in ROOT_NAMES {
        assert_eq!(root_intrinsic(name), Some(*id), "root {name} drifted");
        assert!(
            id.name().starts_with("Core"),
            "root {name} must map to a Core* id, found {}",
            id.name()
        );
        assert_eq!(by_id(*id).name, *name);
    }
    assert_eq!(
        root_intrinsic("project"),
        None,
        "project has no implementation and is not in the spec"
    );
    assert_eq!(root_intrinsic("Some"), Some(IntrinsicId::CoreSome));
    assert_eq!(root_intrinsic("last"), Some(IntrinsicId::CoreLast));
}

#[test]
fn pin_ranges_are_sane() {
    assert!(PinRange::CURRENT.contains(CURRENT_STD_PIN));
    assert!(!PinRange::HISTORICAL_V4_V5.contains(CURRENT_STD_PIN));
    assert!(PinRange::CURRENT.is_current());
    assert!(!PinRange::HISTORICAL_V4_V5.is_current());

    let mut current = 0;
    let mut historical = 0;
    for intrinsic in catalogue() {
        assert!(!intrinsic.pins.since.is_empty());
        if intrinsic.pins.is_current() {
            current += 1;
            assert_eq!(intrinsic.pins.since, CURRENT_STD_PIN);
        } else {
            historical += 1;
            assert_eq!(intrinsic.pins, PinRange::HISTORICAL_V4_V5);
        }
    }
    assert_eq!(
        historical, 6,
        "the six historical-pin leaves must be tagged"
    );
    assert_eq!(current, catalogue().len() - historical);
}

#[test]
fn module_paths_and_notes_are_consistent() {
    for intrinsic in catalogue() {
        match intrinsic.module {
            Module::Core => assert_eq!(intrinsic.module.path(), ""),
            module => assert!(
                intrinsic.module.path().starts_with("std."),
                "{} is not a std module",
                module.name()
            ),
        }
    }
}

#[test]
fn render_is_deterministic_and_nested_types_round_trip() {
    let nested = SigType::List(&SigType::Option(&SigType::Tuple(&[
        SigType::Instant,
        SigType::Param(0),
    ])));
    assert_eq!(nested.render(), "[(Instant, Param(0))?]");
    assert_eq!(
        SigType::Fn(&[SigType::Param(0)], &SigType::Bool).render(),
        "fn(Param(0)): Bool"
    );
    assert_eq!(
        SigType::Record(&[("locale", SigType::Str), ("time_zone", SigType::Str)]).render(),
        "{ locale: Str, time_zone: Str }"
    );
}

#[test]
fn descriptors_are_copy_and_comparable() {
    let intrinsic: Intrinsic = *by_id(IntrinsicId::CoreCount);
    assert_eq!(intrinsic, *by_id(IntrinsicId::CoreCount));
    assert_eq!(intrinsic.name, "count");
    assert_eq!(intrinsic.module, Module::Collection);
}
