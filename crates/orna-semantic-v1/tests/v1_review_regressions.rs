//! Executable audit counterexamples for ornadb-gov5.17.12.
//!
//! Confirmed external reference: https://github.com/workingdir/ornadb/issues/780
//! Normative references below are relative to reference/Orna-1.0.0/.
//! Ignored tests assert required rejection, never acceptance of a known gap.
//! Run them explicitly with `--ignored`; skipped cases are not conformance passes.
//!
//! Inputs are in-memory modules with logical paths: no files, temp directories,
//! environment changes, fixture catalogues, or runtime evaluation are needed.

use orna_semantic_v1::{Analysis, DIAG_TYPE, DIAG_UNRESOLVED, ModuleInput, analyze};

fn analyze_main(source: &str) -> Analysis {
    analyze(&[ModuleInput::new("main.orna", source)])
}

fn expect_diagnostics(result: &Analysis, expected_codes: &[&str]) {
    // These are the public API's semantic classifications, not new spec codes.
    // Requiring only the expected classifications excludes accidental parse,
    // import, and unsupported-syntax failures from satisfying a regression.
    assert!(
        !result.is_ok(),
        "expected semantic rejection with {expected_codes:?}, got no diagnostics"
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|diagnostic| expected_codes.contains(&diagnostic.code())),
        "expected only {expected_codes:?}, got {:#?}",
        result.diagnostics
    );
}

fn expect_accepted(result: &Analysis) {
    assert!(result.is_ok(), "{:#?}", result.diagnostics);
}

// source/05-types.md:455-465, Algorithm INFER-1 steps 3-6:
// constrain contextual types and assignments, solve, reject incompatibility.
#[test]
#[ignore = "Known Orna 1.0.0 gap: #780; run explicitly for review"]
fn local_annotated_initializer_mismatch_requires_type_diagnostic() {
    let result = analyze_main(
        r#"
            pub fn bad(): Int {
                let x: Int = "wrong";
                x
            }
        "#,
    );
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

#[test]
fn compatible_local_annotated_initializers_are_accepted() {
    let result = analyze_main(
        r#"
            pub fn integer(): Int {
                let x: Int = 1;
                x
            }
            pub fn text(): Str {
                let x: Str = "text";
                x
            }
        "#,
    );
    expect_accepted(&result);
}

// Diagnostic control for the same Int/Str conflict at a checked boundary.
#[test]
fn incompatible_function_return_reports_type_diagnostic() {
    let result = analyze_main(r#"pub fn bad(): Int = "wrong";"#);
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

// source/06-expressions.md:11, ORNA-MODULE-003: resolve declarations.
// source/05-types.md:461, Algorithm INFER-1 step 4: resolve exported signatures.
#[test]
#[ignore = "Known Orna 1.0.0 gap: #780; run explicitly for review"]
fn undeclared_annotation_name_requires_unresolved_diagnostic() {
    let result = analyze_main("pub fn identity(x: Missing): Missing = x;");
    expect_diagnostics(&result, &[DIAG_UNRESOLVED]);
}

#[test]
fn declared_nominal_annotation_name_is_accepted() {
    let result = analyze_main(
        r#"
            pub type Box { pub value: Int, }
            pub fn identity(x: Box): Box = x;
        "#,
    );
    expect_accepted(&result);
}

// Diagnostic control: unresolved value names already have a classification.
#[test]
fn undeclared_value_name_reports_unresolved_diagnostic() {
    let result = analyze_main("pub fn bad(): Int = missing;");
    expect_diagnostics(&result, &[DIAG_UNRESOLVED]);
}

// source/05-types.md:368-378, ORNA-NOMINAL-002 and -005 through -007:
// nominal identity survives inference; private-by-default fields are not
// accessible to unrelated modules, including through inferred factory returns.
//
// This preserves the reviewed counterexample. It also violates -007 at the
// factory's construction outside the owning type. Rejection can therefore
// establish a representation violation without proving the consumer access
// itself was checked. Do not close the privacy finding from this test alone.
#[test]
#[ignore = "Known Orna 1.0.0 gap: #780; run explicitly for review"]
fn private_nominal_field_through_factory_requires_semantic_diagnostic() {
    let result = analyze(&[
        ModuleInput::new(
            "vault.orna",
            r#"
                pub type Vault { value: Int, }
                pub fn expose() = Vault { value: 7 };
            "#,
        ),
        ModuleInput::new(
            "main.orna",
            r#"
                use vault;
                pub fn leak(): Int = vault.expose().value;
            "#,
        ),
    ]);
    // A private representation may be rejected as an invalid type operation
    // or as a field unavailable to resolution; no exact wording is prescribed.
    expect_diagnostics(&result, &[DIAG_TYPE, DIAG_UNRESOLVED]);
}

// The only semantic change from the negative input is field visibility.
// This validates module imports, nominal construction, factory inference,
// and chained access without depending on an internal signature encoding.
#[test]
fn public_nominal_field_through_factory_is_accepted() {
    let result = analyze(&[
        ModuleInput::new(
            "vault.orna",
            r#"
                pub type Vault { pub value: Int, }
                pub fn expose() = Vault { value: 7 };
            "#,
        ),
        ModuleInput::new(
            "main.orna",
            r#"
                use vault;
                pub fn leak(): Int = vault.expose().value;
            "#,
        ),
    ]);
    expect_accepted(&result);
}

// source/06-expressions.md:181, ORNA-GENERIC-011: every required member
// must have exactly one compatible implementation in the nested impl.
#[test]
#[ignore = "Known Orna 1.0.0 gap: #780; run explicitly for review"]
fn missing_nested_impl_member_requires_type_diagnostic() {
    let result = analyze_main(
        r#"
            pub protocol P {
                fn value(self): Int;
            }
            pub type Box {
                impl P {}
            }
        "#,
    );
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

// ORNA-GENERIC-011. The body matches its own Str annotation, so the
// conflict is specifically between the member and the protocol signature.
#[test]
#[ignore = "Known Orna 1.0.0 gap: #780; run explicitly for review"]
fn wrong_nested_impl_return_signature_requires_type_diagnostic() {
    let result = analyze_main(
        r#"
            pub protocol P {
                fn value(self): Int;
            }
            pub type Box {
                impl P {
                    fn value(self): Str = "wrong";
                }
            }
        "#,
    );
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

// ORNA-GENERIC-011. Return types and the body agree; the input type alone
// differs, so checking only the implementation's return type is insufficient.
#[test]
#[ignore = "Known Orna 1.0.0 gap: #780; run explicitly for review"]
fn wrong_nested_impl_parameter_signature_requires_type_diagnostic() {
    let result = analyze_main(
        r#"
            pub protocol P {
                fn value(self, input: Int): Int;
            }
            pub type Box {
                impl P {
                    fn value(self, input: Str): Int = 1;
                }
            }
        "#,
    );
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

#[test]
fn compatible_nested_impl_signatures_are_accepted() {
    for source in [
        r#"
            pub protocol P {
                fn value(self): Int;
            }
            pub type Box {
                impl P {
                    fn value(self): Int = 1;
                }
            }
        "#,
        r#"
            pub protocol P {
                fn value(self, input: Int): Int;
            }
            pub type Box {
                impl P {
                    fn value(self, input: Int): Int = input;
                }
            }
        "#,
    ] {
        expect_accepted(&analyze_main(source));
    }
}
