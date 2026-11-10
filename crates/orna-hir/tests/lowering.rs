//! Lowering, resolution and call classification over real source.
//!
//! Each test parses source with the real front end, lowers it, and asserts what
//! the HIR holds: definitions, exports, resolved names, argument slots and the
//! classified callee. Every program must also pass [`Program::validate`],
//! because lowering is total and a validation failure is a lowering bug.

use orna_hir::{
    ArgValue, Callee, DefKind, ExprKind, ModulePath, Name, NameTarget, Program, SourceId,
    SourceUnit, lower,
};
use orna_syntax::{SyntaxTree, parse_module_with_file};

fn parse(source: &str) -> SyntaxTree {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    parsed.value
}

fn lower_one(source: &str) -> Program {
    let tree = parse(source);
    let path = ModulePath::new([Name::new("main")]);
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path,
    }]);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

fn module_of(program: &Program, path: &str) -> orna_hir::ModuleId {
    program
        .modules
        .iter_enumerated()
        .find(|(_, module)| {
            module
                .path
                .segments()
                .last()
                .is_some_and(|name| name.as_str() == path)
        })
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("module `{path}` exists"))
}

fn defined(program: &Program, path: &str, name: &str) -> orna_hir::DefId {
    let module = &program.modules[module_of(program, path)];
    module
        .items
        .iter()
        .copied()
        .find(|def| program.defs[*def].name.as_str() == name)
        .unwrap_or_else(|| panic!("`{name}` is declared in `{path}`"))
}

#[test]
fn functions_become_definitions_with_body_calls() {
    let program = lower_one("fn helper(x: Int): Int = x + 1;\npub fn run(): Int = helper(41);\n");
    let helper = defined(&program, "main", "helper");
    let run = defined(&program, "main", "run");
    assert!(matches!(program.defs[helper].kind, DefKind::Fn(_)));
    let module = &program.modules[module_of(&program, "main")];
    assert_eq!(module.exports.get(&Name::new("run")), Some(&run));
    assert_eq!(module.exports.get(&Name::new("helper")), None);

    let body = program.source_body(run).expect("run has a body");
    let root = &program.bodies[body].exprs[program.bodies[body].root];
    let ExprKind::Call(call) = &root.kind else {
        panic!("run's root is a call, got {:?}", root.kind);
    };
    assert!(matches!(call.callee, Callee::Source { def, .. } if def == helper));
    assert_eq!(call.args.len(), 1);
    assert_eq!(call.args[0].slot.as_u32(), 0);
    assert!(!call.piped);
}

#[test]
fn a_qualified_call_resolves_through_the_module_graph() {
    let a = parse("pub fn helper(x: Int): Int = x;\n");
    let b = parse("use a;\npub fn run(): Int = a.helper(1);\n");
    let program = lower(&[
        SourceUnit {
            source: SourceId::new(0),
            tree: &a,
            path: ModulePath::new([Name::new("a")]),
        },
        SourceUnit {
            source: SourceId::new(1),
            tree: &b,
            path: ModulePath::new([Name::new("b")]),
        },
    ]);
    assert_eq!(program.validate(), Ok(()));
    let helper = defined(&program, "a", "helper");
    let module_b = module_of(&program, "b");
    let run = *program.modules[module_b]
        .exports
        .get(&Name::new("run"))
        .expect("run is exported");
    let body = program.source_body(run).expect("run has a body");
    let root = &program.bodies[body].exprs[program.bodies[body].root];
    let ExprKind::Call(call) = &root.kind else {
        panic!("run's root is a call");
    };
    assert!(matches!(call.callee, Callee::Source { def, .. } if def == helper));
    assert!(
        program.modules[module_b]
            .name_uses
            .iter()
            .any(|use_| use_.name.as_str() == "helper" && use_.target == NameTarget::Def(helper))
    );
}

#[test]
fn an_unresolved_name_is_a_diagnostic_and_an_error_expression() {
    let program = lower_one("fn run(): Int = missing;\n");
    let module = &program.modules[module_of(&program, "main")];
    assert!(
        module
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ORNA-S012-UNRESOLVED"),
        "{:?}",
        module.diagnostics
    );
    let run = defined(&program, "main", "run");
    let body = program.source_body(run).expect("run has a body");
    let root = &program.bodies[body].exprs[program.bodies[body].root];
    assert!(matches!(root.kind, ExprKind::Error));
    assert!(
        module
            .name_uses
            .iter()
            .any(|use_| use_.name.as_str() == "missing" && use_.target == NameTarget::Unresolved)
    );
}

#[test]
fn a_piped_value_binds_parameter_zero() {
    let program = lower_one("fn f(x: Int, y: Int): Int = x + y;\nfn run(): Int = 1 | f(2);\n");
    let run = defined(&program, "main", "run");
    let body = program.source_body(run).expect("run has a body");
    let root = &program.bodies[body].exprs[program.bodies[body].root];
    let ExprKind::Call(call) = &root.kind else {
        panic!("run's root is a call");
    };
    assert!(call.piped);
    assert_eq!(call.args.len(), 2);
    assert_eq!(call.args[0].slot.as_u32(), 0);
    assert_eq!(call.args[1].slot.as_u32(), 1);
}

#[test]
fn named_arguments_and_defaults_bind_to_slots() {
    let program = lower_one("fn f(a: Int, b: Int = 2): Int = a + b;\nfn run(): Int = f(a: 1);\n");
    let run = defined(&program, "main", "run");
    let body = program.source_body(run).expect("run has a body");
    let root = &program.bodies[body].exprs[program.bodies[body].root];
    let ExprKind::Call(call) = &root.kind else {
        panic!("run's root is a call");
    };
    assert_eq!(call.args.len(), 2);
    assert!(matches!(call.args[0].value, ArgValue::Expr(_)));
    assert_eq!(call.args[1].value, ArgValue::Default);
}

#[test]
fn a_wildcard_ambiguity_is_reported() {
    let one = parse("pub fn shared(): Int = 1;\n");
    let two = parse("pub fn shared(): Int = 2;\n");
    let user = parse("use one.*;\nuse two.*;\nfn run(): Int = shared();\n");
    let program = lower(&[
        SourceUnit {
            source: SourceId::new(0),
            tree: &one,
            path: ModulePath::new([Name::new("one")]),
        },
        SourceUnit {
            source: SourceId::new(1),
            tree: &two,
            path: ModulePath::new([Name::new("two")]),
        },
        SourceUnit {
            source: SourceId::new(2),
            tree: &user,
            path: ModulePath::new([Name::new("user")]),
        },
    ]);
    assert_eq!(program.validate(), Ok(()));
    let module = &program.modules[module_of(&program, "user")];
    assert!(
        module
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "ORNA-S011-AMBIGUOUS"),
        "{:?}",
        module.diagnostics
    );
}

#[test]
fn a_table_operation_is_classified_once() {
    let program = lower_one(
        "table Note(id: Int, body: Str) { }\nfn run(): Int = Note.insert({ id: 1, body: \"x\" });\n",
    );
    let note = defined(&program, "main", "Note");
    let run = defined(&program, "main", "run");
    let body = program.source_body(run).expect("run has a body");
    let root = &program.bodies[body].exprs[program.bodies[body].root];
    let ExprKind::Call(call) = &root.kind else {
        panic!("run's root is a call");
    };
    assert!(matches!(
        call.callee,
        Callee::TableOp { table, op: orna_hir::TableOp::Insert } if table == note
    ));
}

#[test]
fn a_local_call_is_a_first_class_value() {
    let program = lower_one(
        "fn twice(f: fn(Int): Int, v: Int): Int = f(v);\nfn run(): Int = twice(x => x + 1, 41);\n",
    );
    let run = defined(&program, "main", "run");
    let body = program.source_body(run).expect("run has a body");
    let root = &program.bodies[body].exprs[program.bodies[body].root];
    let ExprKind::Call(call) = &root.kind else {
        panic!("run's root is a call");
    };
    assert!(matches!(call.callee, Callee::Source { .. }));
    let twice = defined(&program, "main", "twice");
    let twice_body = program.source_body(twice).expect("twice has a body");
    let twice_root = &program.bodies[twice_body].exprs[program.bodies[twice_body].root];
    let ExprKind::Call(inner) = &twice_root.kind else {
        panic!("twice's root is a call");
    };
    assert!(matches!(inner.callee, Callee::Value { .. }));
}

#[test]
fn name_uses_are_in_source_order() {
    let program = lower_one("fn f(a: Int): Int = a + f(a);\n");
    let module = &program.modules[module_of(&program, "main")];
    assert!(module.name_uses.len() >= 3, "{:?}", module.name_uses);
    for pair in module.name_uses.windows(2) {
        assert!(
            (pair[0].span.start, pair[0].span.end) <= (pair[1].span.start, pair[1].span.end),
            "name uses are sorted by span: {:?}",
            module.name_uses
        );
    }
}

#[test]
fn root_intrinsics_classify_without_a_declaration() {
    let program = lower_one("fn run(): Int? = Some(1);\n");
    let run = defined(&program, "main", "run");
    let body = program.source_body(run).expect("run has a body");
    let root = &program.bodies[body].exprs[program.bodies[body].root];
    let ExprKind::Call(call) = &root.kind else {
        panic!("run's root is a call");
    };
    assert!(matches!(
        call.callee,
        Callee::Intrinsic(orna_hir::IntrinsicForm::OptionSome)
    ));
}

#[test]
fn a_named_function_called_through_a_value_joins_its_effects() {
    let program = lower_one(
        "table Note(id: Int, body: Str) { }\nfn reader(): Int = Note.count();\nfn run(): Int = (reader)();\n",
    );
    let run = defined(&program, "main", "run");
    let DefKind::Fn(function) = &program.defs[run].kind else {
        panic!("run is a function");
    };
    assert!(
        !function.effects.is_opaque(),
        "a named callee resolves statically"
    );
    assert!(
        function
            .effects
            .kinds()
            .any(|kind| kind == orna_hir::EffectKind::TableRead)
    );
}
