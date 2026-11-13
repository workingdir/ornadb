//! `Lowering` grows a program one batch of units at a time. A batch lowers only the
//! items it declares, and any number of batches lowers to the program one batch
//! lowers to.

use orna_hir::{Lowering, ModulePath, Name, PreludeExports, Program, SourceId, SourceUnit, lower};
use orna_syntax::parse_module_with_file;
use orna_syntax_ast::SyntaxTree;

const LIB: &str = "pub fn increment(x: Int): Int = x + 1;\n";
const CLIENT: &str = "use lib.{increment};\npub fn run(): Int = increment(41);\n";

/// The lowering-order id types whose numbers depend on how units are batched.
const ID_TAGS: [&str; 7] = [
    "DefId", "Ty", "BodyId", "ExprId", "LocalId", "PatId", "ModuleId",
];

fn parse(source: &str) -> SyntaxTree {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    parsed.value
}

fn path(segment: &str) -> ModulePath {
    ModulePath::new([Name::new(segment)])
}

/// `text` with every number inside one of [`ID_TAGS`] removed, so `DefId(509)` reads `DefId(..)`.
fn without_ids(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('(') {
        let (head, tail) = rest.split_at(at);
        out.push_str(head);
        out.push('(');
        let after = &tail[1..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        let numbered = ID_TAGS.iter().any(|tag| head.ends_with(tag));
        if numbered && digits > 0 && after.as_bytes().get(digits) == Some(&b')') {
            rest = &after[digits..];
        } else {
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// The module path, name and kind of every definition, sorted.
///
/// Lowering numbers declarations, nested members, types and bodies as it reaches
/// them, so the numbers depend on how the units were batched. The numbers are removed
/// and the order is discarded; the set of definitions must match.
fn fingerprint(program: &Program) -> Vec<String> {
    let mut lines: Vec<String> = program
        .defs
        .iter()
        .map(|def| {
            format!(
                "{} {} {}",
                program.modules[def.module].path,
                def.name.as_str(),
                without_ids(&format!("{:?}", def.kind))
            )
        })
        .collect();
    lines.sort();
    lines
}

fn assert_clean(program: &Program) {
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    let diagnostics: Vec<_> = program.diagnostics().collect();
    assert!(
        diagnostics.is_empty(),
        "unexpected diagnostics: {diagnostics:?}"
    );
}

#[test]
fn batches_lower_to_the_program_of_one_batch() {
    let lib = parse(LIB);
    let client = parse(CLIENT);
    let lib_unit = SourceUnit {
        source: SourceId::new(0),
        tree: &lib,
        path: path("lib"),
    };
    let client_unit = SourceUnit {
        source: SourceId::new(1),
        tree: &client,
        path: path("client"),
    };

    let batch = lower(&[lib_unit.clone(), client_unit.clone()]);

    let mut lowering = Lowering::new(&PreludeExports::default());
    lowering.extend(std::slice::from_ref(&lib_unit));
    lowering.extend(std::slice::from_ref(&client_unit));
    let incremental = lowering.into_program();

    assert_clean(&batch);
    assert_clean(&incremental);
    assert_eq!(fingerprint(&incremental), fingerprint(&batch));
}

#[test]
fn a_later_batch_lowers_only_its_own_items() {
    let lib = parse(LIB);
    let client = parse(CLIENT);
    let mut lowering = Lowering::new(&PreludeExports::default());
    assert_eq!(
        lowering.lowered_items(),
        0,
        "declaring the embedded modules lowers nothing"
    );

    lowering.extend(&[SourceUnit {
        source: SourceId::new(0),
        tree: &lib,
        path: path("lib"),
    }]);
    let after_lib = lowering.lowered_items();
    assert!(after_lib > 0, "the first batch lowers its items");

    lowering.extend(&[]);
    assert_eq!(
        lowering.lowered_items(),
        after_lib,
        "an empty batch lowers nothing"
    );

    lowering.extend(&[SourceUnit {
        source: SourceId::new(1),
        tree: &client,
        path: path("client"),
    }]);
    assert!(
        lowering.lowered_items() > after_lib,
        "the second batch lowers its own items"
    );
}

#[test]
fn a_snapshot_leaves_the_lowering_open_for_more_units() {
    let lib = parse(LIB);
    let client = parse(CLIENT);
    let lib_unit = SourceUnit {
        source: SourceId::new(0),
        tree: &lib,
        path: path("lib"),
    };
    let client_unit = SourceUnit {
        source: SourceId::new(1),
        tree: &client,
        path: path("client"),
    };

    let mut lowering = Lowering::new(&PreludeExports::default());
    lowering.extend(std::slice::from_ref(&lib_unit));
    let snapshot = lowering.snapshot();
    assert_eq!(
        fingerprint(&snapshot),
        fingerprint(&lower(std::slice::from_ref(&lib_unit))),
        "a snapshot is the program of the units it was taken after"
    );

    lowering.extend(std::slice::from_ref(&client_unit));
    let finished = lowering.into_program();

    assert_eq!(
        fingerprint(&finished),
        fingerprint(&lower(&[lib_unit, client_unit]))
    );
}

#[test]
fn a_later_batch_resolves_names_from_an_earlier_batch() {
    let lib = parse(LIB);
    let client = parse(CLIENT);
    let mut lowering = Lowering::new(&PreludeExports::default());
    lowering.extend(&[SourceUnit {
        source: SourceId::new(0),
        tree: &lib,
        path: path("lib"),
    }]);
    lowering.extend(&[SourceUnit {
        source: SourceId::new(1),
        tree: &client,
        path: path("client"),
    }]);

    assert_clean(&lowering.into_program());
}

#[test]
fn a_later_batch_adds_items_to_an_existing_module() {
    let first = parse("pub fn one(): Int = 1;\n");
    let second = parse("pub fn two(): Int = 2;\n");
    let both = parse("pub fn one(): Int = 1;\npub fn two(): Int = 2;\n");

    let mut lowering = Lowering::new(&PreludeExports::default());
    lowering.extend(&[SourceUnit {
        source: SourceId::new(0),
        tree: &first,
        path: path("lib"),
    }]);
    lowering.extend(&[SourceUnit {
        source: SourceId::new(1),
        tree: &second,
        path: path("lib"),
    }]);
    let incremental = lowering.into_program();

    let one_batch = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &both,
        path: path("lib"),
    }]);

    assert_clean(&incremental);
    assert_eq!(fingerprint(&incremental), fingerprint(&one_batch));
}
