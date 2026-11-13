//! Currency suffixes and `Money` constructors (hir-host.md B4, slice HB2).
//!
//! `12.34.GBP` and `Money<GBP>(12.34)` lower to a `Money` constructor call with the literal kept exact, and the call
//! expression carries the type `Money<GBP>`. A `Float` literal keeps its binary64 payload; its rejection is HB4's.

use orna_hir::{
    ArgValue, Callee, DefId, ExprKind, Lit, ModulePath, Name, Program, SourceId, SourceUnit,
    TyKind, lower,
};
use orna_syntax::parse_module_with_file;
use orna_syntax_ast::SyntaxTree;

const GBP: &str = "pub type GBP {\n    impl Currency {\n        static code = \"GBP\";\n        static minor_digits = 2;\n    }\n}\n";

fn parse(source: &str) -> SyntaxTree {
    let parsed = parse_module_with_file(source, "currency.orna");
    assert!(
        parsed.is_ok(),
        "test source must parse: {:?}",
        parsed.diagnostics
    );
    parsed.value
}

fn lower_one(source: &str) -> Program {
    let tree = parse(source);
    let program = lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path: ModulePath::new([Name::new("main")]),
    }]);
    assert_eq!(program.validate(), Ok(()), "lowered program must validate");
    program
}

fn defined(program: &Program, name: &str) -> DefId {
    let module = program
        .modules
        .iter()
        .find(|module| module.path == ModulePath::new([Name::new("main")]))
        .expect("module `main` exists");
    module
        .items
        .iter()
        .copied()
        .find(|def| program.defs[*def].name.as_str() == name)
        .unwrap_or_else(|| panic!("`{name}` is declared in `main`"))
}

/// The root expression of `run`'s body and the body it lives in.
fn root_of(program: &Program, run: DefId) -> (&orna_hir::Expr, orna_hir::BodyId) {
    let body = program.source_body(run).expect("run has a body");
    (&program.bodies[body].exprs[program.bodies[body].root], body)
}

/// The `Money` constructor's definition, found by its `core.money` path.
fn money_def(program: &Program) -> DefId {
    program
        .defs
        .iter_enumerated()
        .find(|(_, def)| {
            def.name.as_str() == "Money" && matches!(def.kind, orna_hir::DefKind::Type(_))
        })
        .map(|(id, _)| id)
        .expect("core.money declares Money")
}

/// The single constructor argument of a `Money` call, and the call's type argument as a type name.
fn money_call(program: &Program, run: DefId) -> (&orna_hir::Call, &Lit, Option<String>) {
    let (root, body) = root_of(program, run);
    let ExprKind::Call(call) = &root.kind else {
        panic!("a currency literal lowers to a call, got {:?}", root.kind);
    };
    let Callee::Ctor { def } = call.callee else {
        panic!(
            "a currency literal calls a constructor, got {:?}",
            call.callee
        );
    };
    assert_eq!(
        def,
        money_def(program),
        "the constructor is core.money's Money"
    );
    assert_eq!(call.args.len(), 1);
    let ArgValue::Expr(value) = call.args[0].value else {
        panic!("the literal is the argument");
    };
    let ExprKind::Lit(lit) = &program.bodies[body].exprs[value].kind else {
        panic!("the argument is a literal");
    };
    let ty = program.tys.kind(root.ty).expect("the call has a type");
    let TyKind::Adt(owner, args) = ty else {
        panic!("the call is typed as a nominal type, got {ty:?}");
    };
    assert_eq!(*owner, def);
    let currency = args.first().and_then(|arg| match program.tys.kind(*arg)? {
        TyKind::Adt(currency, _) => Some(program.defs[*currency].name.as_str().to_owned()),
        _ => None,
    });
    (call, lit, currency)
}

#[test]
fn a_decimal_suffix_lowers_to_an_exact_money_constructor() {
    let program = lower_one(&format!("{GBP}pub fn run(): Money<GBP> = 12.34.GBP;\n"));
    let run = defined(&program, "run");
    let (_, lit, currency) = money_call(&program, run);
    let Lit::Decimal {
        coefficient,
        exponent,
    } = lit
    else {
        panic!("12.34 is kept as an exact decimal, got {lit:?}");
    };
    assert_eq!(coefficient.to_string(), "1234");
    assert_eq!(exponent.to_string(), "-2");
    assert_eq!(currency.as_deref(), Some("GBP"));
}

#[test]
fn an_integer_suffix_keeps_the_integer_exact() {
    let program = lower_one(&format!("{GBP}pub fn run(): Money<GBP> = 10.GBP;\n"));
    let run = defined(&program, "run");
    let (_, lit, currency) = money_call(&program, run);
    assert!(
        matches!(lit, Lit::Int(value) if value.to_string() == "10"),
        "got {lit:?}"
    );
    assert_eq!(currency.as_deref(), Some("GBP"));
}

#[test]
fn a_float_suffix_keeps_the_float_literal_for_the_rejection() {
    let program = lower_one(&format!("{GBP}pub fn run(): Money<GBP> = 12.34f.GBP;\n"));
    let run = defined(&program, "run");
    let (_, lit, _) = money_call(&program, run);
    assert_eq!(*lit, Lit::Float(12.34f64.to_bits()));
}

#[test]
fn a_money_constructor_call_takes_its_currency_from_the_type_argument() {
    let program = lower_one(&format!("{GBP}pub fn run(): Money<GBP> = Money<GBP>(5);\n"));
    let run = defined(&program, "run");
    let (_, lit, currency) = money_call(&program, run);
    assert!(
        matches!(lit, Lit::Int(value) if value.to_string() == "5"),
        "got {lit:?}"
    );
    assert_eq!(currency.as_deref(), Some("GBP"));
}

#[test]
fn a_suffix_that_names_no_type_is_still_unresolved() {
    let program = lower_one("pub fn run(): Int = 10.Nope;\n");
    let run = defined(&program, "run");
    let (root, _) = root_of(&program, run);
    assert!(!matches!(root.kind, ExprKind::Call(_)));
}
