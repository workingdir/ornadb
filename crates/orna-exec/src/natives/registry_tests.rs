//! The registry and the `core` declarations agree. Every engine native a `core` interface declares is bound by the
//! registry, except the ones this stage leaves unported, and the registry binds only declared natives.

use std::collections::BTreeSet;

use orna_hir::{
    DefKind, FnBody, ModulePath, Name, NativeBinding, NativeDecl, Program, SourceId, SourceUnit,
    lower,
};
use orna_syntax::parse_module_with_file;

use crate::link;

/// Declarations of `core.collection` the registry does not bind yet. Calling one reports that it is not ported.
const UNPORTED_COLLECTION: &[&str] = &["bucket_by"];

fn lowered() -> Program {
    let source = "use core.collection;\npub fn run(): Int = 0;\n";
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path: ModulePath::new([Name::new("main")]),
    }])
}

/// The engine native declaration a definition carries, if it is one.
fn engine_decl(kind: &DefKind) -> Option<&NativeDecl> {
    let DefKind::Fn(function) = kind else {
        return None;
    };
    let FnBody::Native(decl) = &function.body else {
        return None;
    };
    matches!(decl.binding, NativeBinding::Engine { .. }).then_some(decl)
}

/// The names of the engine natives declared in `module`.
fn declared(program: &Program, module: &str) -> BTreeSet<String> {
    program
        .defs
        .iter()
        .filter_map(|def| engine_decl(&def.kind))
        .filter(|decl| decl.key.module.to_string() == module)
        .map(|decl| decl.key.name.as_str().to_owned())
        .collect()
}

/// The names of the engine natives in `module` that the link step binds to a registry implementation.
fn bound(program: &Program, module: &str) -> BTreeSet<String> {
    link::bind(program)
        .keys()
        .filter_map(|id| engine_decl(&program.defs[*id].kind))
        .filter(|decl| decl.key.module.to_string() == module)
        .map(|decl| decl.key.name.as_str().to_owned())
        .collect()
}

#[test]
fn every_declared_collection_native_is_bound_except_the_unported_ones() {
    let program = lowered();
    let declared = declared(&program, "core.collection");
    let bound = bound(&program, "core.collection");
    assert!(
        !declared.is_empty(),
        "core.collection declares engine natives"
    );
    assert!(
        bound.is_subset(&declared),
        "the registry binds only declared natives"
    );
    let unbound: BTreeSet<String> = declared.difference(&bound).cloned().collect();
    let expected: BTreeSet<String> = UNPORTED_COLLECTION
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(unbound, expected);
}
