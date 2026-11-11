//! `sys.explain` over relation queries and functions: the operator tree the executor runs, read from the HIR without
//! evaluating anything.

use orna_exec::explain::{
    Callback, Operator, OperatorKind, explain_function, explain_query, render_function,
    render_operator,
};
use orna_hir::{
    Body, BodyId, Callee, DefId, DefKind, EffectKind, EffectSet, ExprId, ExprKind, FnBody,
    ModulePath, Name, Program, RelOp, SourceId, SourceUnit, lower, typeck,
};
use orna_syntax::parse_module_with_file;

fn lower_one(source: &str) -> Program {
    let parsed = parse_module_with_file(source, "test.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    let mut program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &parsed.value,
        path: ModulePath::new([Name::new("main")]),
    }]);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    typeck(&mut program);
    assert_eq!(program.validate(), Ok(()), "typed program must validate");
    program
}

const NOTE: &str = "pub table Note(id: Int) {\n    text: Str,\n}\n";

fn def_named(program: &Program, text: &str) -> DefId {
    program
        .defs
        .iter_enumerated()
        .find(|(_, def)| def.name.as_str() == text)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("no definition named {text}"))
}

fn body_of(program: &Program, def: DefId) -> BodyId {
    match &program.defs[def].kind {
        DefKind::Fn(function) => match function.body {
            FnBody::Source(body) => body,
            FnBody::Native(_) | FnBody::Signature => {
                panic!("{} has no Orna body", program.defs[def].name.as_str())
            }
        },
        _ => panic!("{} is not a function", program.defs[def].name.as_str()),
    }
}

/// The outermost stage call of `op` in `body`: the one no other stage reads as its input.
fn outermost_stage(body: &Body, op: RelOp) -> ExprId {
    let stages: Vec<ExprId> = body
        .exprs
        .iter_enumerated()
        .filter(|(_, expr)| matches!(&expr.kind, ExprKind::Call(call) if matches!(call.callee, Callee::Relational { op: found } if found == op)))
        .map(|(id, _)| id)
        .collect();
    let is_input = |candidate: ExprId| {
        body.exprs.iter().any(|expr| match &expr.kind {
            ExprKind::Call(call) => {
                matches!(call.callee, Callee::Relational { .. })
                    && call.args.iter().any(|argument| {
                        argument.slot.as_u32() == 0
                            && argument.value == orna_hir::ArgValue::Expr(candidate)
                    })
            }
            _ => false,
        })
    };
    let roots: Vec<ExprId> = stages.into_iter().filter(|id| !is_input(*id)).collect();
    assert_eq!(roots.len(), 1, "exactly one outermost {op:?} stage");
    roots[0]
}

fn explain_stage(source: &str, op: RelOp) -> (Program, Operator) {
    let program = lower_one(source);
    let body = body_of(&program, def_named(&program, "run"));
    let root = outermost_stage(&program.bodies[body], op);
    let operator = explain_query(&program, &program.bodies[body], root);
    (program, operator)
}

#[test]
fn take_over_a_table_shows_the_take_above_the_scan() {
    let source = format!("{NOTE}pub fn run() {{\n    Note.take(10)\n}}\n");
    let (program, root) = explain_stage(&source, RelOp::Take);

    assert_eq!(root.kind, OperatorKind::Take);
    assert_eq!(root.table, None);
    assert_eq!(root.callback, None);
    assert_eq!(root.effects, EffectSet::empty());
    assert_eq!(root.inputs.len(), 1);

    let scan = &root.inputs[0];
    assert_eq!(scan.kind, OperatorKind::Scan);
    assert_eq!(scan.table, Some(def_named(&program, "Note")));
    assert_eq!(scan.effects, EffectSet::single(EffectKind::TableRead));
    assert!(scan.inputs.is_empty());

    assert_eq!(
        render_operator(&program, &root),
        "take effects=pure\n  scan Note effects=table_read\n"
    );
}

#[test]
fn a_chain_of_stages_nests_each_stage_over_the_one_before_it() {
    let source = format!(
        "{NOTE}pub fn run() {{\n    Note.filter(row => row.id > 1).map(row => row.text).take(2)\n}}\n"
    );
    let (program, root) = explain_stage(&source, RelOp::Take);

    assert_eq!(root.kind, OperatorKind::Take);
    let map = &root.inputs[0];
    assert_eq!(map.kind, OperatorKind::Map);
    assert!(matches!(map.callback, Some(Callback::Lambda(_))));
    let filter = &map.inputs[0];
    assert_eq!(filter.kind, OperatorKind::Filter);
    assert!(matches!(filter.callback, Some(Callback::Lambda(_))));
    let scan = &filter.inputs[0];
    assert_eq!(scan.kind, OperatorKind::Scan);
    assert_eq!(scan.table, Some(def_named(&program, "Note")));
    assert!(scan.inputs.is_empty());
}

#[test]
fn a_drop_under_a_take_is_reported_as_a_drop_operator() {
    let source = format!("{NOTE}pub fn run() {{\n    Note.drop(1).take(1)\n}}\n");
    let (_, root) = explain_stage(&source, RelOp::Take);

    assert_eq!(root.kind, OperatorKind::Take);
    assert_eq!(root.inputs[0].kind, OperatorKind::Drop);
    assert_eq!(root.inputs[0].inputs[0].kind, OperatorKind::Scan);
}

#[test]
fn a_stage_over_a_call_reads_its_input_as_an_external_operator() {
    let source = format!(
        "{NOTE}pub fn rows() {{\n    Note.filter(row => row.id > 1)\n}}\npub fn run() {{\n    rows().take(1)\n}}\n"
    );
    let (_, root) = explain_stage(&source, RelOp::Take);

    assert_eq!(root.kind, OperatorKind::Take);
    let external = &root.inputs[0];
    assert_eq!(external.kind, OperatorKind::External);
    assert_eq!(external.effects, EffectSet::opaque());
    assert!(external.inputs.is_empty());
}

#[test]
fn a_named_callback_reports_its_definition() {
    let source = format!(
        "{NOTE}pub fn keep(row: Note): Bool {{\n    row.id > 1\n}}\npub fn run() {{\n    Note.filter(keep)\n}}\n"
    );
    let (program, root) = explain_stage(&source, RelOp::Filter);

    assert_eq!(root.kind, OperatorKind::Filter);
    assert_eq!(
        root.callback,
        Some(Callback::Def(def_named(&program, "keep")))
    );
    assert_eq!(root.inputs[0].kind, OperatorKind::Scan);
}

#[test]
fn explaining_a_function_reports_its_signature_effects_and_tables() {
    let source = format!("{NOTE}pub fn run() {{\n    Note.count()\n}}\n");
    let program = lower_one(&source);
    let run = def_named(&program, "run");
    let explain = explain_function(&program, run).expect("run is a function");

    assert!(
        explain.signature.starts_with("run("),
        "signature was {}",
        explain.signature
    );
    assert!(explain.effects.contains(EffectKind::TableRead));
    assert_eq!(explain.tables, vec![def_named(&program, "Note")]);
    assert!(render_function(&program, &explain).contains("tables: Note"));
}

#[test]
fn a_function_reports_the_tables_of_the_functions_it_calls() {
    let source = format!(
        "{NOTE}pub fn first() {{\n    Note.take(1)\n}}\npub fn run() {{\n    first()\n}}\n"
    );
    let program = lower_one(&source);
    let explain =
        explain_function(&program, def_named(&program, "run")).expect("run is a function");

    assert_eq!(explain.tables, vec![def_named(&program, "Note")]);
}

#[test]
fn a_pure_function_reports_no_tables() {
    let source = "pub fn run(): Int {\n    1 + 2\n}\n";
    let program = lower_one(source);
    let explain =
        explain_function(&program, def_named(&program, "run")).expect("run is a function");

    assert!(explain.tables.is_empty());
    assert!(render_function(&program, &explain).contains("tables: none"));
}

#[test]
fn explaining_a_non_function_reports_nothing() {
    let program = lower_one(&format!("{NOTE}pub fn run(): Int {{\n    1\n}}\n"));
    assert_eq!(
        explain_function(&program, def_named(&program, "Note")),
        None
    );
}
