//! Executable audit counterexamples for ornadb-gov5.17.12.
//!
//! Confirmed external reference: https://github.com/workingdir/ornadb/issues/780
//! Normative references below use the frozen specification's requirement IDs.
//! These tests assert required rejection, never acceptance of a known gap.
//! The candidate intentionally retains residuals for generic substitution and
//! bounds, imported protocol members, full runtime conversion dispatch, and
//! arbitrary default equivalence. The three former ignored probes are active
//! regressions.
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

// Algorithm INFER-1 steps 3-6:
// constrain contextual types and assignments, solve, reject incompatibility.
#[test]
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

// ORNA-MODULE-003 and Algorithm INFER-1 step 4: resolve exported signatures.
#[test]
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

// ORNA-NOMINAL-002 and -005 through -007:
// nominal identity survives inference; private-by-default fields are not
// accessible to unrelated modules, including through inferred factory returns.
//
// This preserves the reviewed counterexample while keeping construction valid
// in the owning module. Rejection must come from private field access in the
// importing module.
#[test]
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

// ORNA-GENERIC-011: every required member
// must have exactly one compatible implementation in the nested impl.
#[test]
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

#[test]
fn unresolved_or_nonprotocol_nested_impl_identity_requires_type_diagnostic() {
    for source in [
        r#"
            pub type Box {
                impl Missing {}
            }
        "#,
        r#"
            pub type Other { value: Int, }
            pub type Box {
                impl Other {}
            }
        "#,
    ] {
        expect_diagnostics(&analyze_main(source), &[DIAG_TYPE]);
    }

    // Display is the narrow builtin presentation residual; it has no local
    // protocol member AST for this validator to resolve.
    expect_accepted(&analyze_main(
        r#"
            pub type Box {
                value: Str,
                impl Display {
                    fn display(self): Str = self.value;
                }
            }
        "#,
    ));
}

// ORNA-GENERIC-011. The body matches its own Str annotation, so the
// conflict is specifically between the member and the protocol signature.
#[test]
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
        r#"
            pub protocol P {
                fn value(self, input): Int;
            }
            pub type Box {
                impl P {
                    fn value(self, input: Int): Int = input;
                }
            }
        "#,
        r#"
            pub protocol P {
                fn value(self, input: Int): Int;
            }
            pub type Box {
                impl P {
                    fn value(self, input) = input;
                }
            }
        "#,
        r#"
            pub protocol P {
                fn value(self, input: Int = 1): Int;
            }
            pub type Box {
                impl P {
                    fn value(self, input: Int = 1): Int = input;
                }
            }
        "#,
        r#"
            pub protocol P {
                fn value(self, input: Int);
            }
            pub type Box {
                impl P {
                    fn value(self, input): Int = input;
                }
            }
        "#,
    ] {
        expect_accepted(&analyze_main(source));
    }
}

#[test]
fn unconstrained_nonself_nested_impl_parameter_requires_type_diagnostic() {
    let rejected = analyze_main(
        r#"
            pub protocol P {
                fn value(self, input): Int;
            }
            pub type Box {
                impl P {
                    fn value(self, input): Int = 1;
                }
            }
        "#,
    );
    // The annotated result does not constrain an otherwise untyped parameter.
    expect_diagnostics(&rejected, &[DIAG_TYPE]);

    let constrained = analyze_main(
        r#"
            pub protocol P {
                fn value(self, input): Int;
            }
            pub type Box {
                impl P {
                    fn value(self, input: Int): Int = input;
                }
            }
        "#,
    );
    expect_accepted(&constrained);
}

#[test]
fn nested_impl_body_can_read_private_target_fields() {
    expect_accepted(&analyze_main(
        r#"
            pub protocol P {
                fn display(self): Str;
            }
            pub type Box {
                value: Str,
                impl P {
                    fn display(self): Str = self.value;
                }
            }
        "#,
    ));
}

#[test]
fn table_nested_impl_body_can_read_private_row_fields() {
    expect_accepted(&analyze_main(
        r#"
            pub protocol P {
                fn display(self): Str;
            }
            pub table Note(id: Int) {
                value: Str,
                impl P {
                    fn display(self): Str = self.value;
                }
            }
        "#,
    ));
}

#[test]
fn table_display_and_present_writes_require_type_diagnostics() {
    for (protocol, member) in [("Display", "display"), ("Present", "present")] {
        let source = format!(
            r#"
                pub protocol {protocol} {{
                    fn {member}(self): Str;
                }}
                pub table Audit {{ message: Str, }}
                pub table Note(id: Int) {{
                    value: Str,
                    impl {protocol} {{
                        fn {member}(self): Str {{
                            Audit.insert({{ message: "displayed" }});
                            self.value
                        }}
                    }}
                }}
            "#,
        );
        expect_diagnostics(&analyze_main(&source), &[DIAG_TYPE]);
    }
}

#[test]
fn presentation_effect_summaries_propagate_through_helpers() {
    let indirect_write = analyze_main(
        r#"
            pub table Audit { message: Str, }
            pub fn helper(): Str {
                Audit.insert({ message: "displayed" });
                "displayed"
            }
            pub type Note {
                value: Str,
                impl Display {
                    fn display(self): Str = helper();
                }
            }
        "#,
    );
    expect_diagnostics(&indirect_write, &[DIAG_TYPE]);

    // The spelling `insert` is not a write when scope resolution identifies a
    // local function with a proven empty effect summary.
    expect_accepted(&analyze_main(
        r#"
            pub fn insert(): Str = "pure";
            pub type Note {
                value: Str,
                impl Display {
                    fn display(self): Str = insert();
                }
            }
        "#,
    ));

    let imported_write = analyze(&[
        ModuleInput::new(
            "helpers.orna",
            r#"
                pub table Audit { message: Str, }
                pub fn helper(): Str {
                    Audit.insert({ message: "displayed" });
                    "displayed"
                }
            "#,
        ),
        ModuleInput::new(
            "main.orna",
            r#"
                use helpers;
                pub type Note {
                    value: Str,
                    impl Display {
                        fn display(self): Str = helpers.helper();
                    }
                }
            "#,
        ),
    ]);
    expect_diagnostics(&imported_write, &[DIAG_TYPE]);

    let imported_pure = analyze(&[
        ModuleInput::new("helpers.orna", r#"pub fn insert(): Str = "pure";"#),
        ModuleInput::new(
            "main.orna",
            r#"
                use helpers;
                pub type Note {
                    value: Str,
                    impl Display {
                        fn display(self): Str = helpers.insert();
                    }
                }
            "#,
        ),
    ]);
    expect_accepted(&imported_pure);

    // Failure is a separate channel from effects. A pure helper may still
    // fail (here through finite-list cardinality) and remains valid in a
    // presenter; only its effect set makes the implementation a write.
    for (protocol, member) in [("Display", "display"), ("Present", "present")] {
        let source = format!(
            r#"
                pub fn maybe(values: [Str]): Str = one(values);
                pub type Note {{
                    value: Str,
                    impl {protocol} {{
                        fn {member}(self): Str = maybe([self.value]);
                    }}
                }}
            "#,
        );
        expect_accepted(&analyze_main(&source));
    }
}

#[test]
fn omitted_nested_impl_annotations_are_checked_contextually() {
    expect_diagnostics(
        &analyze_main(
            r#"
                pub protocol P {
                    fn value(self, input: Int): Int;
                }
                pub type Box {
                    impl P {
                        fn value(self, input) = "wrong";
                    }
                }
            "#,
        ),
        &[DIAG_TYPE],
    );

    expect_diagnostics(
        &analyze_main(
            r#"
                pub protocol P {
                    fn value(self, input: Int = 1): Int;
                }
                pub type Box {
                    impl P {
                        fn value(self, input = "wrong"): Int = input;
                    }
                }
            "#,
        ),
        &[DIAG_TYPE],
    );

    expect_diagnostics(
        &analyze_main(
            r#"
                pub protocol P {
                    fn value(self, input: Int = "wrong"): Int;
                }
                pub type Box {
                    impl P {
                        fn value(self, input: Int = 1): Int = input;
                    }
                }
            "#,
        ),
        &[DIAG_TYPE],
    );

    // Same type is not enough for an omitted argument: the protocol's
    // literal default is part of the member behaviour.
    expect_diagnostics(
        &analyze_main(
            r#"
                pub protocol P {
                    fn value(self, input: Int = 1): Int;
                }
                pub type Box {
                    impl P {
                        fn value(self, input: Int = 2): Int = input;
                    }
                }
            "#,
        ),
        &[DIAG_TYPE],
    );

    // With no protocol result annotation, the implementation's result is
    // still an effective contextual type for its body.
    expect_diagnostics(
        &analyze_main(
            r#"
                pub protocol P {
                    fn value(self);
                }
                pub type Box {
                    impl P {
                        fn value(self): Int = "wrong";
                    }
                }
            "#,
        ),
        &[DIAG_TYPE],
    );
}

#[test]
fn differing_nonliteral_protocol_defaults_fail_closed() {
    let rejected = analyze_main(
        r#"
            pub protocol P {
                fn value(self, input: Int = 1 + 1): Int;
            }
            pub type Box {
                impl P {
                    fn value(self, input: Int = 3): Int = input;
                }
            }
        "#,
    );
    // The implementation may happen to compute the same value, but the
    // validator has no canonical evaluator. Different ASTs are rejected.
    expect_diagnostics(&rejected, &[DIAG_TYPE]);

    let identical = analyze_main(
        r#"
            pub protocol P {
                fn value(self, input: Int = 1 + 1): Int;
            }
            pub type Box {
                impl P {
                    fn value(self, input: Int = 1 + 1): Int = input;
                }
            }
        "#,
    );
    expect_accepted(&identical);
}

#[test]
fn transparent_aliases_resolve_for_protocol_signatures_without_erasing_nominals() {
    let accepted = analyze_main(
        r#"
            type Number = Int;
            pub protocol P {
                fn value(self, input: Number): Number;
            }
            pub type Box {
                impl P {
                    fn value(self, input: Int): Int = input;
                }
            }
        "#,
    );
    expect_accepted(&accepted);

    let rejected = analyze_main(
        r#"
            pub type Token { value: Int, }
            pub protocol P {
                fn value(self, input: Token): Token;
            }
            pub type Box {
                impl P {
                    fn value(self, input: Int): Int = input;
                }
            }
        "#,
    );
    // Transparent aliases may resolve to Int; a nominal Token must remain
    // distinct from Int.
    expect_diagnostics(&rejected, &[DIAG_TYPE]);
}

#[test]
fn local_protocol_aliases_resolve_to_the_declared_protocol_surface() {
    expect_accepted(&analyze_main(
        r#"
            pub protocol P {
                fn value(self): Int;
            }
            type Alias = P;
            pub type Box {
                impl Alias {
                    fn value(self): Int = 1;
                }
            }
        "#,
    ));

    expect_diagnostics(
        &analyze_main(
            r#"
                pub protocol P {
                    fn value(self): Int;
                }
                type Alias = P;
                pub type Box {
                    impl Alias {}
                }
            "#,
        ),
        &[DIAG_TYPE],
    );
}

#[test]
fn generic_protocol_members_remain_outside_this_validator() {
    let result = analyze_main(
        r#"
            pub protocol P {
                fn value<T>(self, input: T): T;
            }
            pub type Box {
                impl P {
                    fn value<T>(self, input: T): T = input;
                }
            }
        "#,
    );
    // This is deliberately not a conformance acceptance claim. Generic
    // member support remains a separate residual; the narrow validator must
    // not report its own missing-member diagnostic for this protocol.
    assert!(result.is_ok(), "{:#?}", result.diagnostics);
}

#[test]
fn unsupported_generic_protocol_instantiations_fail_closed() {
    for source in [
        r#"
            pub protocol P<T> {
                fn value(self): Int;
            }
            pub type Box {
                impl P<Int> {}
            }
        "#,
        r#"
            pub protocol P {
                fn value(self): Int;
            }
            pub type Box {
                impl P<Int> {}
            }
        "#,
        r#"
            pub protocol P<T> {
                fn value(self): Int;
            }
            pub type Box {
                impl P {}
            }
        "#,
    ] {
        expect_diagnostics(&analyze_main(source), &[DIAG_TYPE]);
    }
}

#[test]
fn mixed_generic_protocol_members_keep_their_checkable_surface() {
    let accepted = analyze_main(
        r#"
            pub protocol P {
                fn generic<T>(self, input: T): T;
                fn value(self): Int;
                static label: Str;
            }
            pub type Box {
                impl P {
                    fn value(self): Int = 1;
                    static label = "box";
                }
            }
        "#,
    );
    expect_accepted(&accepted);

    let rejected = analyze_main(
        r#"
            pub protocol P {
                fn generic<T>(self, input: T): T;
                fn value(self): Int;
                static label: Str;
            }
            pub type Box {
                impl P {
                    fn value(self): Int = 1;
                }
            }
        "#,
    );
    expect_diagnostics(&rejected, &[DIAG_TYPE]);

    let generic_implementation = analyze_main(
        r#"
            pub protocol P {
                fn value(self): Int;
            }
            pub type Box {
                impl P {
                    fn value<T>(self): T = 1;
                }
            }
        "#,
    );
    // A generic implementation member cannot satisfy a non-generic required
    // member merely by sharing its name; otherwise substitution is being
    // guessed instead of checked.
    expect_diagnostics(&generic_implementation, &[DIAG_TYPE]);
}

#[test]
fn imported_qualified_protocol_members_remain_a_documented_residual() {
    let result = analyze(&[
        ModuleInput::new(
            "traits.orna",
            r#"
                pub protocol P {
                    fn value(self): Int;
                }
            "#,
        ),
        ModuleInput::new(
            "main.orna",
            r#"
                use traits;
                pub type Box {
                    impl traits.P {}
                }
            "#,
        ),
    ]);
    // The resolved scope retains module/header symbols but not the imported
    // protocol's member AST or stable protocol identity. These forms therefore
    // fail closed; this is not a conformance proof for ORNA-GENERIC-011, and
    // imported-member conformance remains a residual until that data is exposed.
    expect_diagnostics(&result, &[DIAG_TYPE]);

    let named_import = analyze(&[
        ModuleInput::new(
            "traits.orna",
            r#"
                pub protocol P {
                    fn value(self): Int;
                }
            "#,
        ),
        ModuleInput::new(
            "main.orna",
            r#"
                use traits.{P};
                pub type Box {
                    impl P {}
                }
            "#,
        ),
    ]);
    expect_diagnostics(&named_import, &[DIAG_TYPE]);
}

#[test]
fn transparent_aliases_participate_in_protocol_overlap_identity() {
    let result = analyze_main(
        r#"
            type Source = Str;
            pub type Box {
                value: Str,
                impl From<Source> {
                    fn from(value) = Box { value: value };
                }
                impl From<Str> {
                    fn from(value) = Box { value: value };
                }
            }
        "#,
    );
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

#[test]
fn refined_aliases_resolve_transparent_record_shape_for_private_self_access() {
    let result = analyze_main(
        r#"
            type Shape = { value: Str, };
            pub protocol P {
                fn display(self): Str;
            }
            pub type Box = Shape {
                impl P {
                    fn display(self): Str = self.value;
                }
            }
        "#,
    );
    // Box remains nominal; only its transparent record representation is used
    // for the nested implementation's private self.field access.
    expect_accepted(&result);
}

#[test]
fn unparameterized_from_implementation_fails_closed() {
    expect_diagnostics(
        &analyze_main(
            r#"
                pub type Box {
                    impl From {}
                }
            "#,
        ),
        &[DIAG_TYPE],
    );

    expect_accepted(&analyze_main(
        r#"
            pub type Box {
                value: Str,
                impl From<Str> {
                    fn from(value) = Box { value: value };
                }
            }
        "#,
    ));
}

#[test]
fn from_implementation_requires_valid_source_and_from_member() {
    expect_diagnostics(
        &analyze_main(
            r#"
                pub type Box {
                    impl From<Missing> {}
                }
            "#,
        ),
        &[DIAG_TYPE],
    );

    expect_diagnostics(
        &analyze_main(
            r#"
                pub type Box {
                    value: Str,
                    impl From<Str> {}
                }
            "#,
        ),
        &[DIAG_TYPE],
    );

    expect_diagnostics(
        &analyze_main(
            r#"
                pub type Box {
                    value: Str,
                    impl From<Str> {
                        fn from(value: Int) = Box { value: "converted" };
                    }
                }
            "#,
        ),
        &[DIAG_TYPE],
    );

    expect_accepted(&analyze_main(
        r#"
            pub type Box {
                value: Str,
                impl From<Str> {
                    fn from(value) = Box { value: value };
                }
            }
        "#,
    ));
}

#[test]
fn nominal_from_requires_a_nominal_constructor_result() {
    expect_diagnostics(
        &analyze_main(
            r#"
                pub type Box {
                    value: Str,
                    impl From<Str> {
                        fn from(value): Box = { value: value };
                    }
                }
            "#,
        ),
        &[DIAG_TYPE],
    );

    expect_accepted(&analyze_main(
        r#"
            pub type Box {
                value: Str,
                impl From<Str> {
                    fn from(value): Box = Box { value: value };
                }
            }
        "#,
    ));

    // Tables retain their current row construction form: the AST has no
    // nominal table constructor, so a structural row remains supported.
    expect_accepted(&analyze_main(
        r#"
            pub table Note {
                value: Str,
                impl From<Str> {
                    fn from(value): Note = { value: value };
                }
            }
        "#,
    ));
}

#[test]
fn refined_targets_validate_nested_members_and_overlap() {
    for source in [
        r#"
            pub protocol P {
                fn value(self): Int;
            }
            pub type Box = Int {
                impl P {}
            }
        "#,
        r#"
            pub protocol P {
                fn value(self): Int;
            }
            pub type Box = Int {
                impl P {
                    fn value(self): Str = "wrong";
                }
            }
        "#,
        r#"
            pub protocol P {
                fn value(self): Int;
            }
            pub type Box = Int {
                impl P { fn value(self): Int = 1; }
                impl P { fn value(self): Int = 2; }
            }
        "#,
    ] {
        expect_diagnostics(&analyze_main(source), &[DIAG_TYPE]);
    }
}

#[test]
fn nested_impl_member_names_and_parameter_counts_are_checked() {
    for source in [
        r#"
            pub protocol P {
                fn value(self): Int;
            }
            pub type Box {
                impl P {
                    fn value(self): Int = 1;
                    fn value(self): Int = 2;
                }
            }
        "#,
        r#"
            pub protocol P {
                fn value(self): Int;
            }
            pub type Box {
                impl P {
                    fn other(self): Int = 1;
                }
            }
        "#,
        r#"
            pub protocol P {
                fn value(self, input: Int): Int;
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
                    fn value(self, other: Int): Int = other;
                }
            }
        "#,
        r#"
            pub protocol P {
                fn value(self, input: Int = 1): Int;
            }
            pub type Box {
                impl P {
                    fn value(self, input: Int): Int = input;
                }
            }
        "#,
    ] {
        expect_diagnostics(&analyze_main(source), &[DIAG_TYPE]);
    }
}

#[test]
fn nested_impl_static_properties_are_checked() {
    expect_accepted(&analyze_main(
        r#"
            pub protocol P {
                static code: Str;
            }
            pub type Box {
                impl P {
                    static code = "box";
                }
            }
        "#,
    ));

    for source in [
        r#"
            pub protocol P {
                static code: Str;
            }
            pub type Box {
                impl P {}
            }
        "#,
        r#"
            pub protocol P {
                static code: Str;
            }
            pub type Box {
                impl P {
                    static code = 1;
                }
            }
        "#,
    ] {
        expect_diagnostics(&analyze_main(source), &[DIAG_TYPE]);
    }
}
