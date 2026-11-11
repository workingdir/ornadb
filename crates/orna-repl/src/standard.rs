//! The pinned standard library as HIR source units, parsed once per process.
//!
//! Every session input lowers against the whole standard library, so a qualified `std.x.y` path needs no `use`, and
//! an imported `std` module resolves the same way it does in a project.

use std::sync::LazyLock;

use orna_hir::{ModulePath, Name, SourceId, SourceUnit};
use orna_standard_sources::reference_standard_sources_v1;
use orna_syntax::{SyntaxTree, parse_module_with_file};

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

/// The standard units followed by the session unit `session`, which lowers under `path`.
pub(crate) fn units_with<'a>(session: &'a SyntaxTree, path: ModulePath) -> Vec<SourceUnit<'a>> {
    let mut units: Vec<SourceUnit<'a>> = MODULES
        .iter()
        .enumerate()
        .map(|(index, module)| SourceUnit {
            source: source_id(index),
            tree: &module.tree,
            path: module.path.clone(),
        })
        .collect();
    units.push(SourceUnit {
        source: source_id(MODULES.len()),
        tree: session,
        path,
    });
    units
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
