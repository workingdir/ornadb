use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_foundation_v1::CanonicalValue;
use orna_value_v1::Raw;

fn bool_value(value: bool) -> CanonicalValue {
    CanonicalValue::new(Raw::Bool(value)).unwrap()
}

#[test]
fn pinned_math_module_provides_exact_integer_utilities() {
    let math_source = orna_standard::reference_standard_sources_v1()
        .into_iter()
        .find(|(path, _)| path == "std/math.orna")
        .expect("the pinned math module is published")
        .1;
    let parsed = orna_syntax_v1::parse_module_with_file(&math_source, "math.orna");
    assert!(parsed.is_ok(), "{:#?}", parsed.diagnostics);
    let mut session = AdmittedReplSession::with_reference_standard(Limits::default())
        .unwrap_or_else(|error| panic!("{}", error.code()));
    assert_eq!(
        session.submit(include_str!("fixtures/stdlib-use-math-t7auz.orna")),
        Ok(None)
    );
    for clause in include_str!("fixtures/stdlib-math-contract-t7auz.orna").split("&&") {
        let clause = clause.trim();
        let result = session.submit(clause).unwrap_or_else(|error| {
            panic!("math contract clause {clause:?} failed: {}", error.code())
        });
        assert_eq!(result, Some(bool_value(true)), "math clause: {clause}");
    }
}

#[test]
fn core_numeric_operations_need_no_std() {
    let mut session = AdmittedReplSession::new(Limits::default());
    let core = include_str!("fixtures/stdlib-core-numeric-without-t7auz.orna");
    assert_eq!(session.submit(core), Ok(Some(bool_value(true))));
}
