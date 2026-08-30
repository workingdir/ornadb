#[test]
fn v11_snapshot_uses_generic_source_lowering_for_math() {
    let snapshot = super::super::retained_standard_library_v11_snapshot()
        .expect("the retained V11 source is valid");
    let verified = super::super::verify_standard_library_v11_snapshot(snapshot)
        .expect("the retained V11 source verifies");
    let checked = orna_compiler::check_standard_library_source(&verified)
        .expect("the V11 source checks");
    let math_functions = verified
        .catalogue()
        .functions()
        .iter()
        .filter(|function| function.name().parts().get(1).is_some_and(|part| part == "math"))
        .collect::<Vec<_>>();
    assert_eq!(math_functions.len(), 6);
    assert_eq!(checked.checked_executables().len(), 18);
    for function in math_functions {
        let executable = verified
            .executables()
            .iter()
            .find(|executable| executable.function() == function.id())
            .expect("the math function has an executable");
        assert!(!executable.revision().artifact().payload().is_empty());
        assert!(executable
            .references()
            .iter()
            .all(|reference| reference.source_function() == function.id()));
    }
}
