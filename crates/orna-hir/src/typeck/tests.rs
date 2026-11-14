use super::{TypeCheckSummary, typeck};
use crate::units::Rational;
use crate::{
    DefId, DefKind, ExprKind, ModuleId, ModulePath, Name, Program, SourceId, SourceUnit, lower,
    print_type,
};
use orna_syntax::parse_module_with_file;

pub(super) fn program(source: &str) -> Program {
    let tree = parse_module_with_file(source, "test.orna").value;
    lower(&[SourceUnit {
        source: SourceId::new(0),
        tree: &tree,
        path: ModulePath::new([Name::new("test")]),
    }])
}

pub(super) fn definition(program: &Program, name: &str) -> DefId {
    program
        .defs
        .iter_enumerated()
        .find(|(_, definition)| definition.name.as_str() == name)
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("no definition named {name}"))
}

/// The module the test source lowered to. Lowering registers the engine-embedded
/// `core` and `sys` modules first, so the index is not zero.
pub(super) fn test_module(program: &Program) -> ModuleId {
    program
        .modules
        .iter_enumerated()
        .find(|(_, module)| module.path.to_string() == "test")
        .map(|(id, _)| id)
        .expect("the test module is registered")
}

/// Whether every body of the test module types. The embedded `core` and `sys`
/// bodies belong to the interface, not to the program under test, so one
/// program's verdict ignores them.
pub(super) fn test_accepted(program: &Program, summary: &TypeCheckSummary) -> bool {
    let module = test_module(program);
    summary
        .outcomes
        .iter()
        .filter(|outcome| outcome.module == module)
        .all(|outcome| outcome.typed)
}

/// How many bodies of the test module the subset cannot type yet.
fn test_not_yet(program: &Program, summary: &TypeCheckSummary) -> usize {
    let module = test_module(program);
    summary
        .outcomes
        .iter()
        .filter(|outcome| outcome.module == module && outcome.not_yet.is_some())
        .count()
}

pub(super) fn result(program: &Program, name: &str) -> String {
    let definition = definition(program, name);
    match &program.defs[definition].kind {
        DefKind::Fn(function) => print_type(program, function.ret),
        other => panic!("{name} is not a function: {other:?}"),
    }
}

#[test]
fn annotated_arithmetic_and_result_inference() {
    let mut program = program("pub fn add(a: Int, b: Int) = a + b;\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "add"), "Int");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .is_empty()
    );
}

#[test]
fn let_annotations_are_the_initializer_expected_type() {
    let mut program = program("pub fn scaled(x: Int): Int { let y: Int = x + 1; y * 2 }\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "scaled"), "Int");
}

#[test]
fn a_call_is_checked_against_the_resolved_signature() {
    let mut program =
        program("fn twice(x: Int): Int = x * 2;\npub fn f(y: Int): Int = twice(y);\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "f"), "Int");
}

#[test]
fn a_lambda_receives_the_element_type_from_the_pipeline() {
    let mut program =
        program("pub fn inc(values: [Int]): [Int] = values | map(value => value + 1);\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "inc"), "[Int]");
}

#[test]
fn an_empty_list_is_solved_by_its_context() {
    let mut program = program("pub fn f(): [Int] { let xs = []; xs }\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
}

#[test]
fn an_unconstrained_variable_is_an_ambiguity_diagnostic() {
    let mut program = program("pub fn f() { let xs = []; let ys = xs; }\n");
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == crate::diag::INFER_AMBIGUOUS),
        "{:?}",
        program.modules[test_module(&program)].diagnostics
    );
}

#[test]
fn a_constraint_conflict_names_both_sides() {
    let mut program = program("pub fn f(x: Int): Str = x + 1;\n");
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
    let conflict = program.modules[test_module(&program)]
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == crate::diag::INFER_CONFLICT)
        .expect("a conflict is reported");
    assert!(conflict.message.contains("Int"), "{}", conflict.message);
    assert!(conflict.message.contains("Str"), "{}", conflict.message);
}

#[test]
fn a_range_membership_is_typed_by_the_element_type() {
    let mut program = program("pub fn f(value: Int): Bool = value in 1..=5;\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
}

#[test]
fn a_range_membership_with_a_mismatched_element_is_a_conflict() {
    let mut program = program("pub fn f(value: Str): Bool = value in 1..=5;\n");
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
}
#[test]
fn an_enum_constructor_and_case_are_typed() {
    let mut program = program(
        "pub enum Status { draft, published }\n\
             pub fn label(status: Status): Str = case status {\n\
                 Status.draft: \"Draft\",\n\
                 Status.published: \"Published\",\n\
             };\n",
    );
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "label"), "Str");
}

#[test]
fn a_construct_outside_the_subset_is_reported_not_rejected() {
    let mut program = program("pub fn f(): Unit = parallel([1, 2]);\n");
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(test_not_yet(&program, &summary), 1);
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .is_empty(),
        "{:?}",
        program.modules[test_module(&program)].diagnostics
    );
}

#[test]
fn a_printed_signature_uses_the_specification_spelling() {
    let mut program = program("pub fn add(a: Int, b: [Str?]): Int = a;\n");
    typeck(&mut program);
    let definition = definition(&program, "add");
    assert_eq!(
        crate::print_signature(&program, definition).as_deref(),
        Some("add(Int, [Str?]) -> Int")
    );
}

#[test]
fn a_protocol_member_called_on_an_instance_is_typed() {
    let mut program = program(
        "pub protocol P {\n    fn value(self): Int;\n}\npub type Box {\n    impl P {\n        fn value(self): Int = 1;\n    }\n}\npub fn f(b: Box): Int = b.value();\n",
    );
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "f"), "Int");
}

#[test]
fn a_bounded_generic_call_resolves_through_the_protocol() {
    let mut program = program(
        "pub protocol P {\n    fn value(self): Int;\n}\npub fn read<T: P>(x: T): Int = x.value();\n",
    );
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "read"), "Int");
}

#[test]
fn two_implementations_of_one_member_are_an_overlap_diagnostic() {
    let mut program = program(
        "pub protocol P {\n    fn value(self): Int;\n}\npub type Box {\n    impl P {\n        fn value(self): Int = 1;\n    }\n    impl P {\n        fn value(self): Int = 2;\n    }\n}\npub fn f(b: Box): Int = b.value();\n",
    );
    let _ = typeck(&mut program);
    let module = test_module(&program);
    assert!(
        program.modules[module]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == crate::diag::GENERIC_OVERLAP),
        "expected ORNA-GENERIC-014"
    );
}

#[test]
fn a_member_call_without_an_implementation_is_not_typed() {
    let mut program = program(
        "pub protocol P {\n    fn value(self): Int;\n}\npub type Box {\n    x: Int,\n}\npub fn f(b: Box): Int = b.value();\n",
    );
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
}

#[test]
fn an_enum_implementation_static_is_read_through_the_enum() {
    let mut program = program(
        "pub protocol Named { fn name(self): Str; }\n\
             pub enum Rounding {\n\
                 half_up,\n\
                 half_even,\n\
                 impl Named {\n\
                     static label = \"rounding\";\n\
                     fn name(self): Str = \"rounding\";\n\
                 }\n\
             }\n\
             pub fn f(): Str = Rounding.label;\n",
    );
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "f"), "Str");
}

#[test]
fn an_unknown_enum_static_is_not_a_member() {
    let mut program = program(
        "pub protocol Named { fn name(self): Str; }\n\
             pub enum Rounding {\n\
                 half_up,\n\
                 impl Named {\n\
                     static label = \"rounding\";\n\
                     fn name(self): Str = \"rounding\";\n\
                 }\n\
             }\n\
             pub fn f(): Str = Rounding.missing;\n",
    );
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
}

#[test]
fn a_bare_variant_is_named_by_its_expected_enum_type() {
    let mut program =
        program("pub enum Status { draft, published }\npub fn start(): Status = draft;\n");
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .is_empty(),
        "{:?}",
        program.modules[test_module(&program)].diagnostics
    );
}

#[test]
fn a_bare_name_that_is_no_expected_variant_is_unresolved() {
    let mut program =
        program("pub enum Status { draft, published }\npub fn start(): Status = archived;\n");
    let summary = typeck(&mut program);
    assert!(!test_accepted(&program, &summary), "{summary:?}");
    assert!(
        program.modules[test_module(&program)]
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == crate::diag::UNRESOLVED),
        "{:?}",
        program.modules[test_module(&program)].diagnostics
    );
}

const LENGTH: &str =
    "pub enum Length {\n    metre,\n    kilometre = 1000 metre,\n    impl Unit {}\n}\n";

/// Checks `LENGTH` followed by `body`, returning the program and the checker's verdict.
fn typed(body: &str) -> (Program, TypeCheckSummary) {
    let mut program = program(&format!("{LENGTH}{body}"));
    let summary = typeck(&mut program);
    (program, summary)
}

/// The codes of every diagnostic the test module reports.
fn module_codes(program: &Program) -> Vec<&str> {
    program.modules[test_module(program)]
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code)
        .collect()
}

#[test]
fn a_unit_literal_is_a_quantity_of_its_unit() {
    let (program, summary) = typed("pub fn run() = 5.kilometre;\n");
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "run"), "Quantity<Int, Length.kilometre>");
}

#[test]
fn quantities_of_one_unit_add_in_that_unit() {
    let (program, summary) = typed("pub fn run() = 5.kilometre + 2.kilometre;\n");
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "run"), "Quantity<Int, Length.kilometre>");
}

#[test]
fn addition_across_units_takes_the_finer_unit() {
    let (program, summary) = typed("pub fn run() = 5.kilometre + 2.metre;\n");
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "run"), "Quantity<Int, Length.metre>");
}

#[test]
fn a_quantity_and_a_plain_number_are_a_dimension_error() {
    let (program, summary) = typed("pub fn run() = 5.kilometre + 2;\n");
    assert!(!test_accepted(&program, &summary), "{summary:?}");
    assert!(module_codes(&program).contains(&crate::diag::UNIT_DIM));
}

#[test]
fn adding_two_dimensions_is_e5001() {
    let (program, summary) = typed_families("pub fn run() = 5.metre + 2.second;\n");
    assert!(!test_accepted(&program, &summary), "{summary:?}");
    assert!(module_codes(&program).contains(&crate::diag::QUANTITY_DIMENSIONS));
}

#[test]
fn the_quotient_of_one_unit_is_a_plain_number() {
    let (program, summary) = typed("pub fn run() = 10.metre / 2.metre;\n");
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "run"), "Int");
}

#[test]
fn ordering_quantities_of_one_dimension_is_a_bool() {
    let (program, summary) = typed("pub fn run() = 5.kilometre < 2.metre;\n");
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "run"), "Bool");
}

#[test]
fn a_product_of_quantities_is_accepted() {
    let (program, summary) = typed("pub fn run() = 2.metre * 3.metre;\n");
    assert!(test_accepted(&program, &summary), "{summary:?}");
}

const MONEY: &str = "use core.ops.*;\npub type Money {\n    cents: Int,\n    impl Add<Self, Self> {\n        fn add(self, right: Self): Self = right;\n    }\n}\n";

fn module_has_code(program: &Program, code: &str) -> bool {
    program.modules[test_module(program)]
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == code)
}

#[test]
fn user_type_addition_selects_the_add_instance() {
    let mut program = program(&format!(
        "{MONEY}pub fn total(a: Money, b: Money): Money = a + b;\n"
    ));
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "total"), "Money");
}

#[test]
fn missing_add_instance_names_the_type_operator_and_protocol() {
    let mut program = program(
        "pub type Money {\n    cents: Int,\n}\npub fn total(a: Money, b: Money): Money = a + b;\n",
    );
    let _ = typeck(&mut program);
    let module = test_module(&program);
    let message = program.modules[module]
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == crate::diag::INFER_CONFLICT)
        .map(|diagnostic| diagnostic.message.to_string())
        .unwrap_or_default();
    assert!(message.contains("Money"), "{message}");
    assert!(message.contains("`+`"), "{message}");
    assert!(message.contains("`Add`"), "{message}");
}

#[test]
fn two_applicable_add_instances_are_an_overlap_diagnostic() {
    let mut program = program(
        "use core.ops.*;\npub type Money {\n    cents: Int,\n    impl Add<Self, Self> {\n        fn add(self, right: Self): Self = self;\n    }\n    impl Add<Self, Self> {\n        fn add(self, right: Self): Self = right;\n    }\n}\npub fn total(a: Money, b: Money): Money = a + b;\n",
    );
    let _ = typeck(&mut program);
    assert!(module_has_code(&program, crate::diag::GENERIC_OVERLAP));
}

#[test]
fn negation_selects_the_neg_instance() {
    let mut program = program(
        "use core.ops.*;\npub type Money {\n    cents: Int,\n    impl Neg<Self> {\n        fn neg(self): Self = self;\n    }\n}\npub fn flip(a: Money): Money = -a;\n",
    );
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "flip"), "Money");
}

#[test]
fn membership_selects_the_contains_instance_of_the_container() {
    let mut program = program(
        "use core.ops.*;\npub type Bag {\n    count: Int,\n    impl Contains<Int> {\n        fn contains(self, value: Int): Bool = true;\n    }\n}\npub fn has(bag: Bag, value: Int): Bool = value in bag;\n",
    );
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "has"), "Bool");
}

#[test]
fn missing_contains_instance_names_the_in_operator_and_protocol() {
    let mut program = program(
        "pub type Bag {\n    count: Int,\n}\npub fn has(bag: Bag, value: Int): Bool = value in bag;\n",
    );
    let _ = typeck(&mut program);
    let message = program.modules[test_module(&program)]
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == crate::diag::INFER_CONFLICT)
        .map(|diagnostic| diagnostic.message.to_string())
        .unwrap_or_default();
    assert!(message.contains("`in`"), "{message}");
    assert!(message.contains("`Contains`"), "{message}");
}

#[test]
fn ordering_comparison_selects_the_order_instance() {
    let mut program = program(
        "use core.{Ordering};\nuse core.ops.*;\npub type Money {\n    cents: Int,\n    impl Order {\n        fn compare(self, other: Self): Ordering = equal;\n    }\n}\npub fn before(a: Money, b: Money): Bool = a < b;\n",
    );
    let summary = typeck(&mut program);
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "before"), "Bool");
}

#[test]
fn missing_order_instance_names_the_ordering_operator_and_protocol() {
    let mut program = program(
        "pub type Money {\n    cents: Int,\n}\npub fn before(a: Money, b: Money): Bool = a < b;\n",
    );
    let _ = typeck(&mut program);
    let message = program.modules[test_module(&program)]
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == crate::diag::INFER_CONFLICT)
        .map(|diagnostic| diagnostic.message.to_string())
        .unwrap_or_default();
    assert!(message.contains("`<`"), "{message}");
    assert!(message.contains("`Order`"), "{message}");
}

const TIME: &str = "pub enum Time {\n    second,\n    minute = 60 second,\n    hour = 3600 second,\n    impl Unit {}\n}\n";
const SPEED: &str =
    "pub enum Speed {\n    kmh = Length.kilometre / Time.hour,\n    impl Unit {}\n}\n";

/// Checks `LENGTH`, `TIME` and `SPEED` followed by `body`, returning the program and the checker's verdict.
fn typed_families(body: &str) -> (Program, TypeCheckSummary) {
    let mut program = program(&format!("{LENGTH}{TIME}{SPEED}{body}"));
    let summary = typeck(&mut program);
    (program, summary)
}

/// The factors of every unit conversion recorded in the bodies the named function owns.
fn conversion_factors(program: &Program, name: &str) -> Vec<Rational> {
    let owner = definition(program, name);
    program
        .bodies
        .iter()
        .filter(|body| body.owner == owner)
        .flat_map(|body| body.exprs.iter())
        .filter_map(|expr| match &expr.kind {
            ExprKind::UnitConvert { factor, .. } => Some(factor.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_family_annotation_converts_a_member_unit_to_the_base_unit() {
    let (program, summary) = typed_families("pub fn run(): Time = 1.hour;\n");
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "run"), "Time");
    assert_eq!(
        conversion_factors(&program, "run"),
        vec![Rational::from_int(3600)]
    );
}

#[test]
fn the_base_unit_of_a_family_enters_without_a_conversion() {
    let (program, summary) = typed_families("pub fn run(): Time = 2.second;\n");
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert!(conversion_factors(&program, "run").is_empty());
}

#[test]
fn a_let_annotation_converts_a_member_unit_to_the_base_unit() {
    let (program, summary) =
        typed_families("pub fn run(): Time {\n    let d: Time = 1.minute;\n    d\n}\n");
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(
        conversion_factors(&program, "run"),
        vec![Rational::from_int(60)]
    );
}

#[test]
fn a_bare_number_is_not_a_value_of_a_unit_family() {
    let (program, summary) = typed_families("pub fn run(): Time = 5;\n");
    assert!(!test_accepted(&program, &summary), "{summary:?}");
    assert!(module_codes(&program).contains(&crate::diag::UNIT_ANNOT));
}

#[test]
fn a_quantity_outside_the_family_is_rejected() {
    let (program, summary) = typed_families("pub fn run(): Time = 5.kilometre;\n");
    assert!(!test_accepted(&program, &summary), "{summary:?}");
    assert!(module_codes(&program).contains(&crate::diag::UNIT_ANNOT));
}

#[test]
fn a_derived_family_accepts_its_own_quotient_unit_as_written() {
    let (program, summary) = typed_families("pub fn run(): Speed = 10.kilometre / 2.hour;\n");
    assert!(test_accepted(&program, &summary), "{summary:?}");
    assert_eq!(result(&program, "run"), "Speed");
    assert!(conversion_factors(&program, "run").is_empty());
}

#[test]
fn a_relation_stage_on_a_table_path_takes_the_table_as_argument_zero() {
    let mut program = program("table Note(id: Int) { }\npub fn run() = Note.take(0);\n");
    typeck(&mut program);
    let note = definition(&program, "Note");
    let body_id = program
        .source_body(definition(&program, "run"))
        .expect("run has a body");
    let body = &program.bodies[body_id];
    let ExprKind::Call(call) = &body.exprs[body.root].kind else {
        panic!("run's root is a call");
    };
    assert!(matches!(
        call.callee,
        crate::Callee::Relational {
            op: crate::RelOp::Take
        }
    ));
    assert!(call.piped, "a settled relation stage is piped");
    let receiver = call
        .args
        .iter()
        .find(|argument| argument.slot.as_u32() == 0)
        .expect("slot 0 is the receiver");
    assert!(matches!(
        receiver.value,
        crate::ArgValue::Expr(expr) if matches!(body.exprs[expr].kind, ExprKind::Def(def) if def == note)
    ));
}
