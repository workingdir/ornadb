//! The effect pass counts a table read by name (`docs/design/hir.md` section 2.5).
//!
//! A bare table reference is a scan of that table, and a relation stage or pipe over the table reads it through its
//! receiver. A relation stage over a list value reads nothing, and a table write does not read the table it writes.

use orna_hir::{
    DefKind, EffectKind, EffectSet, ModulePath, Name, Program, SourceId, SourceUnit, lower,
};
use orna_syntax::parse_module_with_file;

fn lower_one(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path: ModulePath::new([Name::new("main")]),
    }]);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

/// The effect set of the source function named `name`.
fn effects_of(program: &Program, name: &str) -> EffectSet {
    let def = program
        .defs
        .iter()
        .find(|def| def.name.as_str() == name)
        .unwrap_or_else(|| panic!("`{name}` is defined"));
    let DefKind::Fn(function) = &def.kind else {
        panic!("`{name}` is a function");
    };
    function.effects
}

#[test]
fn a_pipe_over_a_table_reads_the_table() {
    let program = lower_one("table Note(id: Int) { }\npub fn run() = Note | take(0);\n");
    let effects = effects_of(&program, "run");
    assert!(
        !effects.is_opaque(),
        "a relation stage over a table resolves statically"
    );
    assert!(effects.contains(EffectKind::TableRead));
    assert!(effects.may_fail());
}

#[test]
fn a_pipe_over_a_list_parameter_reads_nothing() {
    let program = lower_one("pub fn run(xs: [Int]) = xs | take(0);\n");
    let effects = effects_of(&program, "run");
    assert!(effects.is_empty(), "a list receiver performs no table read");
}

#[test]
fn a_table_write_does_not_read_the_table() {
    let program = lower_one("table Note(id: Int) { }\npub fn run(id: Int) = Note.delete(id);\n");
    let effects = effects_of(&program, "run");
    assert!(effects.contains(EffectKind::TableWrite));
    assert!(!effects.contains(EffectKind::TableRead));
}

#[test]
fn a_named_source_callback_contributes_its_effects() {
    // The callback reads a table; a stage over a list receives it as a value, so its reads must reach the caller.
    let program = lower_one(
        "table Note(id: Int) { }\nfn hot(x: Int): Bool = Note.count() > x;\npub fn run(xs: [Int]) = xs | filter(hot);\n",
    );
    let effects = effects_of(&program, "run");
    assert!(
        effects.contains(EffectKind::TableRead),
        "the callback reads Note"
    );
    assert!(effects.may_fail());
}

#[test]
fn a_named_callback_under_a_map_contributes_its_effects() {
    let program = lower_one(
        "table Note(id: Int) { }\nfn hot(x: Int): Bool = Note.count() > x;\npub fn run(xs: [Int]) = xs | map(hot);\n",
    );
    let effects = effects_of(&program, "run");
    assert!(
        effects.contains(EffectKind::TableRead),
        "the callback reads Note"
    );
}

#[test]
fn a_pure_named_callback_adds_nothing() {
    let program = lower_one(
        "fn small(x: Int): Bool = x < 10;\npub fn run(xs: [Int]) = xs | filter(small);\n",
    );
    let effects = effects_of(&program, "run");
    assert!(effects.is_empty(), "a pure callback performs no effect");
}

#[test]
fn a_function_parameter_given_to_a_stage_is_unknown() {
    // The stage invokes the parameter, whose effects the pass cannot name here: they fail closed.
    let program = lower_one("pub fn run(xs: [Int], keep: fn(Int): Bool) = xs | filter(keep);\n");
    let effects = effects_of(&program, "run");
    assert!(effects.is_opaque(), "an unnamed callback is opaque");
}
