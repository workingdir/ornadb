//! The engine-embedded interface modules `core` and `sys` (design hir.md H2,
//! decision D12).
//!
//! Their sources live under `interface/` and are generated from the old
//! catalogue by `scripts/gen_core_interface.py`. They declare the engine's
//! natives with body-less `native fn`, which bind to the engine registry (a
//! key in these modules is an engine native; see `NativeBinding`). Lowering
//! loads them ahead of every caller's sources, so a native call always has a
//! signature to type against.

use orna_syntax::{SyntaxTree, parse_interface_with_file};

use crate::{ModulePath, Name, NativeAbi, SourceId};

/// The native ABI the engine implements (design 4.5). A program records it when
/// lowering admits the embedded declarations, and the link step refuses a
/// program built against another ABI.
pub const ENGINE_NATIVE_ABI: NativeAbi = NativeAbi::new(1);

/// The first source id given to an embedded module. The ids run upward from
/// here, below the built-in source id that the implicit `std` root uses, so
/// they never collide with a caller's source ids.
pub(crate) const EMBEDDED_SOURCE_BASE: u32 = u32::MAX - 64;

/// Every embedded interface file: its dotted module path and its text.
const FILES: [(&str, &str); 18] = [
    ("core", include_str!("../interface/core.orna")),
    ("core.bits", include_str!("../interface/core/bits.orna")),
    ("core.ops", include_str!("../interface/core/ops.orna")),
    ("core.range", include_str!("../interface/core/range.orna")),
    (
        "core.temperature",
        include_str!("../interface/core/temperature.orna"),
    ),
    ("core.units", include_str!("../interface/core/units.orna")),
    (
        "core.display",
        include_str!("../interface/core/display.orna"),
    ),
    (
        "core.collection",
        include_str!("../interface/core/collection.orna"),
    ),
    (
        "core.encoding.base64",
        include_str!("../interface/core/encoding/base64.orna"),
    ),
    (
        "core.encoding.json",
        include_str!("../interface/core/encoding/json.orna"),
    ),
    (
        "core.encoding.orna",
        include_str!("../interface/core/encoding/orna.orna"),
    ),
    (
        "core.encoding.ovb",
        include_str!("../interface/core/encoding/ovb.orna"),
    ),
    ("core.hash", include_str!("../interface/core/hash.orna")),
    ("core.stats", include_str!("../interface/core/stats.orna")),
    ("core.text", include_str!("../interface/core/text.orna")),
    ("core.time", include_str!("../interface/core/time.orna")),
    ("sys", include_str!("../interface/sys.orna")),
    ("sys.engine", include_str!("../interface/sys/engine.orna")),
];

/// One parsed embedded module.
pub(crate) struct EmbeddedModule {
    /// The module's canonical path.
    pub(crate) path: ModulePath,
    /// The parsed tree.
    pub(crate) tree: SyntaxTree,
}

/// Parses every embedded interface file.
///
/// The files are part of the binary, so a parse diagnostic is a build defect;
/// the test `embedded_interfaces_parse_cleanly` holds that invariant.
pub(crate) fn parse_all() -> Vec<EmbeddedModule> {
    FILES
        .iter()
        .map(|(dotted, text)| {
            let path = ModulePath::new(dotted.split('.').map(Name::new));
            let tree = parse_interface_with_file(text, format!("interface/{dotted}.orna")).value;
            EmbeddedModule { path, tree }
        })
        .collect()
}

/// The parsed embedded modules. They are parsed once per process and shared by every lowering.
pub(crate) fn parsed() -> &'static [EmbeddedModule] {
    static PARSED: std::sync::LazyLock<Vec<EmbeddedModule>> = std::sync::LazyLock::new(parse_all);
    &PARSED
}

/// The source id of the `index`th embedded module.
pub(crate) fn source_id(index: usize) -> SourceId {
    let offset = u32::try_from(index).expect("embedded module count fits u32");
    SourceId::new(EMBEDDED_SOURCE_BASE + offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_interfaces_parse_cleanly() {
        for (dotted, text) in FILES {
            let parsed = parse_interface_with_file(text, dotted);
            assert!(
                parsed.diagnostics.is_empty(),
                "{dotted} has parse diagnostics: {:?}",
                parsed.diagnostics
            );
        }
    }

    #[test]
    fn embedded_modules_cover_every_interface_file() {
        let modules = parse_all();
        assert_eq!(modules.len(), FILES.len());
        assert!(
            modules
                .iter()
                .any(|module| module.path == ModulePath::new([Name::new("sys")]))
        );
        assert!(
            modules
                .iter()
                .any(|module| module.path == ModulePath::new([Name::new("core")]))
        );
    }
}
