//! The pinned standard library as HIR source units, parsed once per process.
//!
//! Every session input lowers against the whole standard library, so a qualified `std.x.y` path needs no `use`, and
//! an imported `std` module resolves the same way it does in a project.

use std::sync::LazyLock;

use orna_hir::{Lowering, ModulePath, Name, PreludeExports, SourceId, SourceUnit};
use orna_standard_sources::{REFERENCE_STANDARD_PRELUDE_EXPORTS_V1, reference_standard_sources_v1};
use orna_syntax::parse_module_with_file;
use orna_syntax_ast::SyntaxTree;

/// One standard module: the path lowering registers it under and its parsed tree.
struct StandardModule {
    path: ModulePath,
    tree: SyntaxTree,
}

static MODULES: LazyLock<Vec<StandardModule>> = LazyLock::new(|| {
    reference_standard_sources_v1()
        .into_iter()
        .map(|(logical, text)| StandardModule {
            path: module_path_of(&logical),
            tree: parse_module_with_file(&text, logical).value,
        })
        .collect()
});

/// The namespace a logical path declares: `std/collection/advanced.orna` is `std.collection.advanced`, and a
/// trailing `main` names its directory, as `ORNA-NS-001` to `ORNA-NS-004` require.
fn module_path_of(logical: &str) -> ModulePath {
    let trimmed = logical.strip_suffix(".orna").unwrap_or(logical);
    let mut segments = trimmed
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if segments.last() == Some(&"main") {
        segments.pop();
    }
    ModulePath::new(segments.into_iter().map(Name::new))
}

/// The standard library lowered once per process, and the source index the session module lowers under: the first
/// index after the standard units.
static LOWERED: LazyLock<(Lowering, u32)> = LazyLock::new(|| {
    let prelude = PreludeExports::default().with_module(
        ModulePath::new([Name::new("std"), Name::new("prelude")]),
        REFERENCE_STANDARD_PRELUDE_EXPORTS_V1.iter().copied(),
    );
    let units: Vec<SourceUnit<'_>> = MODULES
        .iter()
        .enumerate()
        .map(|(index, module)| SourceUnit {
            source: source_id(index),
            tree: &module.tree,
            path: module.path.clone(),
        })
        .collect();
    let mut lowering = Lowering::new(&prelude);
    lowering.extend(&units);
    (
        lowering,
        u32::try_from(MODULES.len()).expect("source index fits u32"),
    )
});

/// The standard library lowered, and the source index the session module lowers under.
pub(crate) fn lowering() -> (Lowering, u32) {
    LOWERED.clone()
}

fn source_id(index: usize) -> SourceId {
    SourceId::new(u32::try_from(index).expect("source index fits u32"))
}

#[cfg(test)]
mod tests {
    use super::module_path_of;

    #[test]
    fn logical_paths_map_to_namespaces() {
        assert_eq!(
            module_path_of("std/prelude.orna").to_string(),
            "std.prelude"
        );
        assert_eq!(
            module_path_of("std/collection/advanced.orna").to_string(),
            "std.collection.advanced"
        );
        assert_eq!(
            module_path_of("std/concurrent/main.orna").to_string(),
            "std.concurrent"
        );
    }
}
