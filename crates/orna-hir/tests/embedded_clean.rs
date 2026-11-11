//! The engine-embedded interface modules lower with zero diagnostics.
//!
//! `core`, every `core.*` module and `sys` are loaded ahead of every caller's
//! sources (`embedded.rs`). A diagnostic on one of them is an interface defect
//! that every program inherits, so this test lowers an empty user program and
//! asserts that no embedded module reports anything.

use orna_hir::{ModuleOrigin, Program, lower};

/// Every module that the engine embeds: `core`, `core.*` and `sys`.
fn embedded_modules(program: &Program) -> Vec<(String, Vec<String>)> {
    program
        .modules
        .iter()
        .filter(|module| matches!(module.origin, ModuleOrigin::Core | ModuleOrigin::Sys))
        .map(|module| {
            let path = module
                .path
                .segments()
                .iter()
                .map(|name| name.as_str().to_owned())
                .collect::<Vec<_>>()
                .join(".");
            let diagnostics = module
                .diagnostics
                .iter()
                .map(|diagnostic| format!("{} {}", diagnostic.code, diagnostic.message))
                .collect();
            (path, diagnostics)
        })
        .collect()
}

#[test]
fn every_embedded_module_lowers_with_zero_diagnostics() {
    let program = lower(&[]);
    let modules = embedded_modules(&program);

    let paths: Vec<&str> = modules.iter().map(|(path, _)| path.as_str()).collect();
    assert!(paths.contains(&"core"), "`core` is embedded: {paths:?}");
    assert!(paths.contains(&"sys"), "`sys` is embedded: {paths:?}");
    assert!(
        paths.iter().any(|path| path.starts_with("core.")),
        "`core.*` modules are embedded: {paths:?}"
    );

    let mut report = String::new();
    for (path, diagnostics) in &modules {
        for diagnostic in diagnostics {
            report.push_str(&format!("  [{path}] {diagnostic}\n"));
        }
    }
    assert!(
        report.is_empty(),
        "embedded modules must lower with zero diagnostics; found {} across {} modules:\n{report}",
        report.lines().count(),
        modules.len(),
    );
}
