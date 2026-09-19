#[test]
fn v11_snapshot_uses_generic_source_lowering_for_math() {
    let snapshot = super::super::retained_standard_library_v11_snapshot()
        .expect("the retained V11 source is valid");
    let verified = super::super::verify_standard_library_v11_snapshot(snapshot)
        .expect("the retained V11 source verifies");
    let checked =
        orna_compiler::check_standard_library_source(&verified).expect("the V11 source checks");
    let math_functions = verified
        .catalogue()
        .functions()
        .iter()
        .filter(|function| {
            function
                .name()
                .parts()
                .get(1)
                .is_some_and(|part| part == "math")
        })
        .collect::<Vec<_>>();
    assert_eq!(math_functions.len(), 6);
    assert_eq!(checked.checked_executables().len(), 18);
    assert_eq!(verified.executables().len(), 18);
    for function in math_functions {
        let executable = verified
            .executables()
            .iter()
            .find(|executable| executable.function() == function.id())
            .expect("the math function has an executable");
        assert!(!executable.revision().artifact().payload().is_empty());
        assert!(
            executable
                .references()
                .iter()
                .all(|reference| reference.source_function() == function.id())
        );
    }
}

#[test]
fn v11_math_dogfood_fixture_checks_and_prepares() {
    let snapshot = super::super::retained_standard_library_v11_snapshot()
        .expect("the retained V11 source is valid");
    let verified = super::super::verify_standard_library_v11_snapshot(snapshot)
        .expect("the retained V11 source verifies");
    let standard =
        orna_compiler::check_standard_library_source(&verified).expect("the V11 source checks");
    let base = super::empty_version_two_active_revision(&verified);
    let context =
        orna_compiler::StandardApplicationCheckContext::try_new(base.catalogue(), &standard)
            .expect("V11 application context");
    let source = include_str!("fixtures/v11_math_dogfood.orna");
    let bundle = orna_core::source::SourceBundle::new([orna_core::source::SourceUnit::new(
        "v11_math_dogfood.orna",
        source,
    )])
    .expect("dogfood bundle");
    let report = orna_compiler::check_standard_application(&bundle, &context);
    assert!(
        report.diagnostics().is_empty(),
        "{:?}",
        report.diagnostics()
    );
    let prepared = orna_compiler::prepare_standard_application(&report, base.pair(), &base)
        .expect("dogfood source prepares");
    assert_eq!(prepared.candidate().functions().len(), 6);
    assert_eq!(prepared.new_function_revisions().len(), 6);
}
#[test]
fn v11_retained_snapshot_rejects_tampered_math_source_hash() {
    let snapshot = super::super::retained_standard_library_v11_snapshot()
        .expect("the retained V11 source is valid");
    let mut units = snapshot.source().units().to_vec();
    let math_index = units
        .iter()
        .position(|unit| unit.logical_path() == super::super::STD_MATH_SOURCE_LOGICAL_PATH)
        .expect("the math unit is present");
    let original = units[math_index].clone();
    units[math_index] = orna_core::revision::StoredSourceUnit::new(
        original.id(),
        original.ordinal(),
        original.logical_path(),
        original.content(),
        orna_core::revision::Sha256Digest::from_bytes([0; 32]),
    )
    .expect("the tampered source unit remains structurally valid");
    let source = orna_core::revision::StoredSourceRevision::new(
        snapshot.source().bundle(),
        snapshot.source().id(),
        snapshot.source().parent(),
        units,
        snapshot.source().bundle_hash(),
        snapshot.source().revision_hash(),
    )
    .expect("the tampered source revision remains structurally valid");
    let tampered_snapshot = orna_core::revision::StandardLibrarySnapshot::new_with_executables(
        snapshot.revision(),
        snapshot.digest_version(),
        source,
        snapshot.language_version(),
        snapshot.catalogue().clone(),
        snapshot.executables().to_vec(),
        snapshot.origins().to_vec(),
        snapshot.digest(),
    )
    .expect("the tampered snapshot remains structurally valid");
    assert!(matches!(
        super::super::verify_standard_library_v11_snapshot(tampered_snapshot),
        Err(super::super::StandardLibraryError::RetainedSourceMismatch)
    ));
}
#[test]
fn v11_verifier_rejects_swapped_math_executable_identity() {
    let snapshot = super::super::retained_standard_library_v11_snapshot()
        .expect("the retained V11 source is valid");
    let mut executables = snapshot.executables().to_vec();
    let first = executables[12].clone();
    let second = executables[13].clone();
    let first_function = first.function();
    let second_function = second.function();
    let first_revision = first.revision().clone();
    let second_revision = second.revision().clone();
    let first_swapped = orna_core::revision::FunctionRevisionRecord::new(
        first_function,
        first_revision.id(),
        first_revision.revision_number(),
        first_revision.declaration_origin(),
        first_revision.declaration_content_hash(),
        second_revision.semantic_hash(),
        second_revision.language_version(),
        second_revision.artifact().clone(),
    )
    .expect("the first tampered revision remains structurally valid")
    .with_semantic_hash_version(second_revision.semantic_hash_version());
    let second_swapped = orna_core::revision::FunctionRevisionRecord::new(
        second_function,
        second_revision.id(),
        second_revision.revision_number(),
        second_revision.declaration_origin(),
        second_revision.declaration_content_hash(),
        first_revision.semantic_hash(),
        first_revision.language_version(),
        first_revision.artifact().clone(),
    )
    .expect("the second tampered revision remains structurally valid")
    .with_semantic_hash_version(first_revision.semantic_hash_version());
    executables[12] = orna_core::revision::StandardExecutable::new(
        first_function,
        first_swapped,
        first.references().to_vec(),
    )
    .expect("the first swapped executable remains structurally valid");
    executables[13] = orna_core::revision::StandardExecutable::new(
        second_function,
        second_swapped,
        second.references().to_vec(),
    )
    .expect("the second swapped executable remains structurally valid");
    let tampered = orna_core::revision::StandardLibrarySnapshot::new_with_executables(
        snapshot.revision(),
        snapshot.digest_version(),
        snapshot.source().clone(),
        snapshot.language_version(),
        snapshot.catalogue().clone(),
        executables,
        snapshot.origins().to_vec(),
        snapshot.digest(),
    )
    .expect("the swapped snapshot remains structurally valid");
    assert!(matches!(
        super::super::verify_standard_library_v11_snapshot(tampered),
        Err(super::super::StandardLibraryError::CanonicalHash { .. })
            | Err(super::super::StandardLibraryError::RetainedSourceMismatch)
    ));
}
#[test]
fn v11_checker_rejects_wrong_math_source_identity_and_ordinal() {
    let snapshot = super::super::retained_standard_library_v11_snapshot()
        .expect("the retained V11 source is valid");
    let mut units = snapshot.source().units().to_vec();
    let math_index = units
        .iter()
        .position(|unit| unit.logical_path() == super::super::STD_MATH_SOURCE_LOGICAL_PATH)
        .expect("the math unit is present");
    let original = units[math_index].clone();
    units[math_index] = orna_core::revision::StoredSourceUnit::new(
        original.id(),
        original.ordinal(),
        "std/wrong-math.orna",
        original.content(),
        original.content_hash(),
    )
    .expect("the malformed source unit remains structurally valid");
    let source = orna_core::revision::StoredSourceRevision::new(
        snapshot.source().bundle(),
        snapshot.source().id(),
        snapshot.source().parent(),
        units,
        snapshot.source().bundle_hash(),
        snapshot.source().revision_hash(),
    )
    .expect("the malformed source revision remains structurally valid");
    let tampered = orna_core::revision::StandardLibrarySnapshot::new_with_executables(
        snapshot.revision(),
        snapshot.digest_version(),
        source,
        snapshot.language_version(),
        snapshot.catalogue().clone(),
        snapshot.executables().to_vec(),
        snapshot.origins().to_vec(),
        snapshot.digest(),
    )
    .expect("the malformed snapshot remains structurally valid");
    let verified = super::super::verify_standard_library_v11_snapshot(tampered)
        .expect_err("the retained verifier must reject the malformed source");
    assert!(matches!(
        verified,
        super::super::StandardLibraryError::RetainedSourceMismatch
    ));
}

#[test]
fn v11_checker_rejects_missing_math_executable() {
    let snapshot = super::super::retained_standard_library_v11_snapshot()
        .expect("the retained V11 source is valid");
    let mut executables = snapshot.executables().to_vec();
    executables.pop();
    let tampered = orna_core::revision::StandardLibrarySnapshot::new_with_executables(
        snapshot.revision(),
        snapshot.digest_version(),
        snapshot.source().clone(),
        snapshot.language_version(),
        snapshot.catalogue().clone(),
        executables,
        snapshot.origins().to_vec(),
        snapshot.digest(),
    )
    .expect_err("the public snapshot constructor must reject a short executable set");
    assert!(matches!(
        tampered,
        orna_core::revision::RevisionInvariantError::StandardExecutableSequenceLengthMismatch { .. }
    ));
}

#[test]
fn v11_verifier_rejects_tampered_library_digest() {
    let snapshot = super::super::retained_standard_library_v11_snapshot()
        .expect("the retained V11 source is valid");
    let tampered = orna_core::revision::StandardLibrarySnapshot::new_with_executables(
        snapshot.revision(),
        snapshot.digest_version(),
        snapshot.source().clone(),
        snapshot.language_version(),
        snapshot.catalogue().clone(),
        snapshot.executables().to_vec(),
        snapshot.origins().to_vec(),
        orna_core::revision::Sha256Digest::from_bytes([0; 32]),
    )
    .expect("the tampered snapshot remains structurally valid");
    assert!(matches!(
        super::super::verify_standard_library_v11_snapshot(tampered),
        Err(super::super::StandardLibraryError::RetainedSourceMismatch)
    ));
}
