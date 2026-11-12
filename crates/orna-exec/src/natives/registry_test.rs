//! The engine registry against the embedded `core` interface: every `pub native fn` the interface declares for a ported
//! family is bound by [`super::lookup`], so no declared native is left to the not-yet-ported stop.

use orna_hir::{ModulePath, Name, NativeKey};

use super::lookup;

/// The embedded interface sources of the families the engine ports, by module path.
const PORTED: &[(&str, &str)] = &[
    (
        "core.bits",
        include_str!("../../../orna-hir/interface/core/bits.orna"),
    ),
    (
        "core.hash",
        include_str!("../../../orna-hir/interface/core/hash.orna"),
    ),
    (
        "core.text",
        include_str!("../../../orna-hir/interface/core/text.orna"),
    ),
    (
        "core.encoding.base64",
        include_str!("../../../orna-hir/interface/core/encoding/base64.orna"),
    ),
];

/// The names of the `pub native fn` declarations one interface source states.
fn declared(source: &str) -> Vec<&str> {
    source
        .lines()
        .filter_map(|line| line.strip_prefix("pub native fn "))
        .map(|rest| rest.split('(').next().unwrap_or_default().trim())
        .filter(|name| !name.is_empty())
        .collect()
}

#[test]
fn every_declared_native_of_a_ported_family_is_bound() {
    for (module, source) in PORTED {
        let names = declared(source);
        assert!(!names.is_empty(), "{module} declares natives");
        for name in names {
            let key = NativeKey {
                module: ModulePath::new(module.split('.').map(Name::new)),
                name: Name::new(name),
            };
            assert!(
                lookup(&key).is_some(),
                "{module}.{name} is declared but has no engine native"
            );
        }
    }
}

#[test]
fn the_text_family_declares_exactly_the_ten_ported_natives() {
    let names = declared(include_str!("../../../orna-hir/interface/core/text.orna"));
    assert_eq!(
        names,
        [
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
        ]
    );
}

/// The `sys` blob read-side natives are bound; `blob_verify` needs the dependency structure and stays unbound.
#[test]
fn the_sys_blob_family_binds_its_read_side_natives() {
    for name in [
        "blob_length",
        "blob_digest",
        "blob_read",
        "blob_from_bytes",
        "blob_to_bytes",
    ] {
        let key = NativeKey {
            module: ModulePath::new(["sys"].into_iter().map(Name::new)),
            name: Name::new(name),
        };
        assert!(lookup(&key).is_some(), "sys.{name} has no engine native");
    }
    let verify = NativeKey {
        module: ModulePath::new(["sys"].into_iter().map(Name::new)),
        name: Name::new("blob_verify"),
    };
    assert!(lookup(&verify).is_none());
}
