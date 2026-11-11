//! The engine-embedded `sys` interface, `crates/orna-hir/interface/sys.orna`, must parse cleanly under the
//! interface grammar. Every native callable is a body-less `native fn` with an effect clause.

use orna_syntax::{Declaration, parse_interface};

const SYS_INTERFACE: &str = include_str!("../../orna-hir/interface/sys.orna");

/// One `native fn` per active catalogue callable (82) plus the two retired callables that carry a `removed` clause.
const SYS_NATIVE_FUNCTIONS: usize = 84;

/// The 1-based line and text of the source at each diagnostic's start offset.
fn located(source: &str, diagnostics: &[orna_syntax::SyntaxDiagnostic]) -> Vec<String> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            let offset = diagnostic.span.start.min(source.len());
            let line = source[..offset].matches('\n').count() + 1;
            let text = source.lines().nth(line - 1).unwrap_or("");
            format!(
                "{}:{} {} [{}]",
                line,
                diagnostic.code,
                diagnostic.message,
                text.trim()
            )
        })
        .collect()
}

#[test]
fn sys_interface_parses_without_diagnostics() {
    let parsed = parse_interface(SYS_INTERFACE);
    assert!(
        parsed.is_ok(),
        "sys.orna should parse, got {:#?}",
        located(SYS_INTERFACE, &parsed.diagnostics)
    );
}

#[test]
fn sys_interface_declares_every_native_function_with_an_effect_clause() {
    let parsed = parse_interface(SYS_INTERFACE);
    let natives: Vec<_> = parsed
        .value
        .items
        .iter()
        .filter_map(|item| match &item.declaration {
            Declaration::Native { effects, .. } => Some(effects),
            _ => None,
        })
        .collect();
    assert_eq!(natives.len(), SYS_NATIVE_FUNCTIONS);
    assert!(natives.iter().all(|effects| effects.is_some()));
}
