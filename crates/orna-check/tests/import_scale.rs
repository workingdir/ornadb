//! Checking a project whose modules each import every other module.
//!
//! The source holds one `use` per ordered pair of modules, so the work grows
//! with the square of the module count by construction. The step that made it
//! grow faster, a scan of every module for each import, is pinned exactly by
//! `enum_bindings_tests` (a visit count, which does not depend on the machine).
//! This test is the wall-clock backstop over the whole analysis: its budget is
//! far above what a debug build needs, so only a step that costs orders of
//! magnitude more per import fails it.

use std::time::{Duration, Instant};

use orna_check::{ModuleInput, analyze};

/// Half of the project loader's default module limit.
const MODULES: usize = 128;
/// About thirty times what the analysis takes on a loaded developer machine.
const BUDGET: Duration = Duration::from_secs(60);

fn module(index: usize, count: usize) -> ModuleInput {
    let imports = (0..count)
        .filter(|other| *other != index)
        .map(|other| format!("use m{other};\n"))
        .collect::<String>();
    ModuleInput::new(
        format!("m{index}.orna"),
        format!(
            "{imports}pub enum Kind{index} {{ first, second, }}\n\
             pub fn value{index}(x: Int): Int = x + {index};\n"
        ),
    )
}

#[test]
fn modules_that_each_import_every_other_module_check_within_budget() {
    let inputs = (0..MODULES)
        .map(|index| module(index, MODULES))
        .collect::<Vec<_>>();

    let started = Instant::now();
    let analysis = analyze(&inputs);
    let elapsed = started.elapsed();

    assert!(
        analysis.diagnostics.is_empty(),
        "{:?}",
        &analysis.diagnostics[..analysis.diagnostics.len().min(3)]
    );
    assert_eq!(analysis.modules.len(), MODULES);
    assert!(
        elapsed < BUDGET,
        "{MODULES} modules importing each other took {elapsed:?}, budget {BUDGET:?}"
    );
}
