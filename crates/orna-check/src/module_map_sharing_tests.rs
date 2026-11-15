use super::*;

/// `count` flat modules that import nothing: every module's analysis sees
/// the whole project in its module map, which is the shape that made
/// project analysis quadratic in module count.
fn flat_project(count: usize) -> Vec<ModuleInput> {
    (0..count)
        .map(|index| ModuleInput {
            logical_path: format!("m{index}.orna"),
            source: format!("pub fn a{index}() = {index};\npub fn b{index}() = a{index}() + 1;\n"),
            prelude_exports: BTreeSet::new(),
        })
        .collect()
}

#[test]
fn analysis_never_deep_copies_the_module_map_per_module() {
    let inputs = flat_project(12);
    reset_module_header_copies();
    let analysis = analyze(&inputs);
    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        analysis.diagnostics
    );
    assert_eq!(module_header_copies(), 0);
}
