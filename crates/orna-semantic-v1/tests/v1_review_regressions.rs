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

use orna_semantic_v1::{
    Analysis, DIAG_ANNOTATION, DIAG_DUPLICATE, DIAG_TYPE, DIAG_UNRESOLVED, ModuleInput, Namespace,
    Type, analyze,
};
use orna_syntax_v1::parse_module_with_file;

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

// ORNA-INFER-002/-007 and ORNA-S020-ANNOTATION: a field shape inferred only
// from an unconstrained lambda body cannot export an internal Type::Error.
#[test]
fn underconstrained_lambda_field_inference_requires_annotation() {
    let underconstrained = analyze_main("pub fn getter() = x => x.value;");
    expect_diagnostics(&underconstrained, &[DIAG_ANNOTATION]);
    assert_eq!(
        underconstrained
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code() == DIAG_ANNOTATION)
            .count(),
        1
    );
    assert!(
        !underconstrained
            .modules
            .get(&Namespace(Vec::new()))
            .expect("main module")
            .exports
            .contains_key("getter")
    );

    let constrained = analyze_main("pub fn increment() = x => x + 1;");
    expect_accepted(&constrained);

    let known_row = analyze_main(
        "pub table Reading(id: Int) { value: Int, } pub fn values() = Reading | map(row => row.value);",
    );
    expect_accepted(&known_row);
}

// ORNA-INFER-002 and -007: an unsupported annotation must reject at its
// declaration boundary rather than become an internal wildcard that permits
// incompatible function bodies or calls.
#[test]
fn unsupported_product_annotation_requires_type_diagnostic() {
    let result = analyze_main(
        r#"
            pub fn bad(value: Int * Int): Bool = value;
            pub fn caller(): Bool = bad("wrong");
        "#,
    );
    expect_diagnostics(&result, &[DIAG_TYPE]);
    assert!(result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.message() == "type product is not a supported static type"));
}

// Diagnostic control for the same Int/Str conflict at a checked boundary.
#[test]
fn incompatible_function_return_reports_type_diagnostic() {
    let result = analyze_main(r#"pub fn bad(): Int = "wrong";"#);
    expect_diagnostics(&result, &[DIAG_TYPE]);
}

#[test]
fn annotated_direct_return_is_checked_and_ends_the_function_body() {
    let result = analyze_main(
        r#"
            pub fn integer(): Int {
                return 1;
                1;
            }
        "#,
    );
    expect_accepted(&result);
}

#[test]
fn direct_return_ends_body_before_later_statement() {
    let result = analyze_main(
        r#"
            pub fn integer() {
                return 1;
                "ignored";
            }
        "#,
    );
    expect_accepted(&result);
    let symbol = result
        .modules
        .values()
        .next()
        .and_then(|module| module.symbols.get("integer"))
        .expect("inferred direct-return function");
    assert!(matches!(
        &symbol.ty,
        Type::Function { result, .. } if result.as_ref() == &Type::Int
    ));
}

#[test]
fn direct_return_mismatch_reports_type_diagnostic() {
    let result = analyze_main(
        r#"
            pub fn bad(): Int {
                return "wrong";
            }
        "#,
    );
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

#[test]
fn qualified_annotation_names_use_imported_module_scope() {
    let accepted = analyze(&[
        ModuleInput::new("vault.orna", "pub type Box { pub value: Int, }"),
        ModuleInput::new(
            "main.orna",
            "use vault; pub fn identity(value: vault.Box): vault.Box = value;",
        ),
    ]);
    expect_accepted(&accepted);

    let undeclared = analyze(&[
        ModuleInput::new("vault.orna", "pub type Box { pub value: Int, }"),
        ModuleInput::new(
            "main.orna",
            "use vault; pub fn bad(value: vault.Missing): vault.Missing = value; pub fn applied(value: List<Missing>): List<Missing> = value;",
        ),
    ]);
    expect_diagnostics(&undeclared, &[DIAG_UNRESOLVED]);

    let not_imported = analyze(&[
        ModuleInput::new("vault.orna", "pub type Box { pub value: Int, }"),
        ModuleInput::new(
            "main.orna",
            "pub fn bad(value: vault.Box): vault.Box = value;",
        ),
    ]);
    expect_diagnostics(&not_imported, &[DIAG_UNRESOLVED]);
}

#[test]
fn generic_and_function_type_annotations_remain_resolvable() {
    let result = analyze_main(
        r#"
            pub fn identity<T>(value: T): T = value;
            pub fn apply(callback: fn(Int): Int): Int = callback(1);
            pub fn measured(value: Float<mph>): Float<mph> = value;
        "#,
    );
    expect_accepted(&result);
}

#[test]
fn dimensional_annotation_arguments_must_resolve() {
    let result = analyze_main(
        "pub fn bad(money: Money<Missing>, float: Float<Missing>, decimal: Decimal<Missing>, integer: Int<Missing>): Int = 0;",
    );
    expect_diagnostics(&result, &[DIAG_UNRESOLVED]);
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

// ORNA-NOMINAL-005/-007: an imported constructor sees only public fields,
// while declaration-backed required-field metadata prevents it from
// manufacturing a value whose private representation is incomplete.
#[test]
fn imported_nominal_constructor_requires_private_fields() {
    let incomplete = analyze(&[
        ModuleInput::new("vault.orna", "pub type Vault { value: Int, }"),
        ModuleInput::new(
            "main.orna",
            "use vault.{Vault}; pub fn forge(): Vault = Vault {};",
        ),
    ]);
    expect_diagnostics(&incomplete, &[DIAG_TYPE]);

    let supplied_private = analyze(&[
        ModuleInput::new("vault.orna", "pub type Vault { value: Int, }"),
        ModuleInput::new(
            "main.orna",
            "use vault.{Vault}; pub fn forge(): Vault = Vault { value: 1 };",
        ),
    ]);
    expect_diagnostics(&supplied_private, &[DIAG_TYPE]);

    let qualified_incomplete = analyze(&[
        ModuleInput::new("vault.orna", "pub type Vault { value: Int, }"),
        ModuleInput::new("main.orna", "use vault; pub fn forge() = vault.Vault {};"),
    ]);
    expect_diagnostics(&qualified_incomplete, &[DIAG_TYPE]);

    let qualified_shadowed = analyze(&[
        ModuleInput::new("vault.orna", "pub type Vault { pub value: Int, }"),
        ModuleInput::new(
            "main.orna",
            "use vault; pub fn forge() { let vault = 1; vault.Vault { value: 1 } }",
        ),
    ]);
    expect_diagnostics(&qualified_shadowed, &[DIAG_UNRESOLVED]);
}

#[test]
fn imported_nominal_constructor_accepts_complete_public_fields_and_defaults() {
    let complete = analyze(&[
        ModuleInput::new("vault.orna", "pub type Vault { pub value: Int, }"),
        ModuleInput::new(
            "main.orna",
            "use vault.{Vault}; pub fn forge(): Vault = Vault { value: 1 };",
        ),
    ]);
    expect_accepted(&complete);

    let defaulted = analyze(&[
        ModuleInput::new("vault.orna", "pub type Vault { pub value: Int = 1, }"),
        ModuleInput::new(
            "main.orna",
            "use vault.{Vault}; pub fn forge(): Vault = Vault {};",
        ),
    ]);
    expect_accepted(&defaulted);

    let local =
        analyze_main("pub type Vault { value: Int = 1, } pub fn forge(): Vault = Vault {};");
    expect_accepted(&local);

    let qualified = analyze(&[
        ModuleInput::new("vault.orna", "pub type Vault { pub value: Int, }"),
        ModuleInput::new(
            "main.orna",
            "use vault; pub fn forge() = vault.Vault { value: 1 };",
        ),
    ]);
    expect_accepted(&qualified);

    let aliased = analyze(&[
        ModuleInput::new("vault.orna", "pub type Vault { pub value: Int, }"),
        ModuleInput::new(
            "main.orna",
            "use vault as v; pub fn forge() = v.Vault { value: 1 };",
        ),
    ]);
    expect_accepted(&aliased);

    let colliding_short_names = analyze(&[
        ModuleInput::new("alpha.orna", "pub type Vault { pub alpha: Int, }"),
        ModuleInput::new("beta.orna", "pub type Vault { pub beta: Int, }"),
        ModuleInput::new(
            "main.orna",
            "use beta.{Vault}; pub fn forge(): Vault = Vault { beta: 1 };",
        ),
    ]);
    expect_accepted(&colliding_short_names);

    let private_default_supplied = analyze(&[
        ModuleInput::new("vault.orna", "pub type Vault { value: Int = 1, }"),
        ModuleInput::new(
            "main.orna",
            "use vault.{Vault}; pub fn forge(): Vault = Vault { value: 1 };",
        ),
    ]);
    expect_diagnostics(&private_default_supplied, &[DIAG_TYPE]);

    let local_private_required =
        analyze_main("pub type Vault { value: Int, } pub fn forge(): Vault = Vault { value: 1 };");
    expect_accepted(&local_private_required);

    let exported = analyze(&[ModuleInput::new(
        "vault.orna",
        "pub type Vault { value: Int, }",
    )]);
    let vault = exported
        .modules
        .get(&Namespace(vec!["vault".into()]))
        .and_then(|module| module.exports.get("Vault"))
        .expect("exported nominal type");
    let schema = vault.table_schema.as_ref().expect("nominal schema");
    let admission = schema.admission.as_ref().expect("nominal admission");
    assert!(!schema.fields.contains_key("value"));
    assert!(!admission.required.contains("value"));
    assert!(admission.private_required);
}

#[test]
fn nominal_constructor_rejects_duplicate_fields() {
    let result = analyze_main("type T { value: Int, } fn forge() = T { value: 1, value: 2 };");
    expect_diagnostics(&result, &[DIAG_DUPLICATE]);

    let first_value_is_checked =
        analyze_main("type T { value: Int, } fn forge() = T { value: \"wrong\", value: 2 };");
    expect_diagnostics(&first_value_is_checked, &[DIAG_DUPLICATE, DIAG_TYPE]);
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
fn nested_impl_direct_return_uses_member_result_context() {
    let result = analyze_main(
        r#"
            pub protocol P {
                fn value(self): Int;
            }
            pub type Box {
                impl P {
                    fn value(self): Int {
                        return 1;
                        "ignored";
                    }
                }
            }
        "#,
    );
    expect_accepted(&result);
}

#[test]
fn nested_impl_direct_return_mismatch_reports_one_type_diagnostic() {
    let result = analyze_main(
        r#"
            pub protocol P {
                fn value(self): Int;
            }
            pub type Box {
                impl P {
                    fn value(self): Int {
                        return "wrong";
                    }
                }
            }
        "#,
    );
    assert_eq!(
        result.diagnostics.len(),
        1,
        "expected one diagnostic, got {:#?}",
        result.diagnostics
    );
    assert_eq!(result.diagnostics[0].code(), DIAG_TYPE);
}

#[test]
fn unreachable_break_and_tail_after_direct_return_are_ignored() {
    let result = analyze_main(
        r#"
            pub fn integer(): Int {
                return 1;
                break;
                "ignored";
            }
        "#,
    );
    expect_accepted(&result);
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
fn presentation_direct_return_terminates_write_scan() {
    for (protocol, member) in [("Display", "display"), ("Present", "present")] {
        let accepted = format!(
            r#"
                pub protocol {protocol} {{
                    fn {member}(self): Str;
                }}
                pub table Audit {{ message: Str, }}
                pub table Note(id: Int) {{
                    value: Str,
                    impl {protocol} {{
                        fn {member}(self): Str {{
                            return self.value;
                            Audit.insert({{ message: "unreachable" }});
                        }}
                    }}
                }}
            "#,
        );
        expect_accepted(&analyze_main(&accepted));

        let rejected = format!(
            r#"
                pub protocol {protocol} {{
                    fn {member}(self): Str;
                }}
                pub table Audit {{ message: Str, }}
                pub table Note(id: Int) {{
                    value: Str,
                    impl {protocol} {{
                        fn {member}(self): Str {{
                            Audit.insert({{ message: "before return" }});
                            return self.value;
                        }}
                    }}
                }}
            "#,
        );
        expect_diagnostics(&analyze_main(&rejected), &[DIAG_TYPE]);
    }
}

#[test]
fn presentation_nested_control_return_does_not_terminate_write_scan() {
    let result = analyze_main(
        r#"
            pub protocol Display {
                fn display(self): Str;
            }
            pub table Audit { message: Str, }
            pub table Note(id: Int) {
                value: Str,
                impl Display {
                    fn display(self): Str {
                        if true {
                            return self.value;
                            Audit.insert({ message: "after nested return" });
                        }
                        self.value
                    }
                }
            }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "nested control-flow write must retain the type diagnostic: {:#?}",
        result.diagnostics
    );
}

#[test]
fn presentation_lambda_block_return_does_not_terminate_write_scan() {
    let result = analyze_main(
        r#"
            pub protocol Display {
                fn display(self): Str;
            }
            pub table Audit { message: Str, }
            pub table Note(id: Int) {
                value: Str,
                impl Display {
                    fn display(self): Str {
                        let render = () => {
                            return self.value;
                            Audit.insert({ message: "after lambda return" });
                        };
                        render()
                    }
                }
            }
        "#,
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "nested lambda write must retain the type diagnostic: {:#?}",
        result.diagnostics
    );
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
fn local_generic_protocol_bounds_accept_a_satisfying_call_and_substitute_result() {
    let result = analyze_main(
        r#"
            type Ordering = Int;
            pub protocol Display {
                fn display(self): Str;
            }
            pub protocol Order {
                fn compare(self, other: Self): Ordering;
            }
            pub type Ranked {
                value: Int,
                impl Display {
                    fn display(self): Str = "ranked";
                }
                impl Order {
                    fn compare(self, other: Self): Int = self.value - other.value;
                }
            }
            pub fn keep<T impl Display + Order>(value: T): T = value;
            pub fn accepted(): Ranked = keep(Ranked { value: 1 });
        "#,
    );
    expect_accepted(&result);
    let module = result.modules.values().next().expect("generic module");
    assert_eq!(
        module
            .symbols
            .get("accepted")
            .expect("accepted function")
            .ty,
        orna_semantic_v1::Type::Function {
            parameters: Vec::new(),
            parameter_names: Some(Vec::new()),
            result: Box::new(orna_semantic_v1::Type::Named("Ranked".into())),
            default_parameters: Default::default(),
        }
    );
}

#[test]
fn self_result_keeps_nominal_target_identity() {
    let result = analyze_main(
        r#"
            pub protocol Reproduce {
                fn reproduce(self): Self;
            }
            pub type Box {
                value: Int,
                impl Reproduce {
                    fn reproduce(self): Self = { value: self.value };
                }
            }
        "#,
    );
    expect_diagnostics(&result, &[DIAG_TYPE]);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message() == "static types are incompatible" })
    );
}

#[test]
fn local_generic_protocol_bounds_reject_a_missing_implementation() {
    let result = analyze_main(
        r#"
            pub protocol Display {
                fn display(self): Str;
            }
            pub protocol Order {
                fn compare(self, other: Self): Int;
            }
            pub type DisplayOnly {
                value: Int,
                impl Display {
                    fn display(self): Str = "display-only";
                }
            }
            pub fn keep<T impl Display + Order>(value: T): T = value;
            pub fn rejected(): DisplayOnly = keep(DisplayOnly { value: 1 });
        "#,
    );
    expect_diagnostics(&result, &[DIAG_TYPE]);
    assert!(result.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "generic type argument does not satisfy its protocol bound"
    }));
}

#[test]
fn generic_protocol_overlap_rejection_is_independent_of_source_order() {
    for implementations in [
        r#"
            impl Display { fn display(self): Str = "first"; }
            impl Display { fn display(self): Str = "second"; }
        "#,
        r#"
            impl Display { fn display(self): Str = "second"; }
            impl Display { fn display(self): Str = "first"; }
        "#,
    ] {
        let result = analyze_main(&format!(
            "pub type Repeated {{ value: Int, {implementations} }}"
        ));
        expect_diagnostics(&result, &[DIAG_TYPE]);
        assert!(result.diagnostics.iter().any(|diagnostic| {
            diagnostic.message() == "overlapping protocol implementations are invalid"
        }));
    }
}

#[test]
fn colon_generic_bounds_receive_the_frozen_legacy_diagnostic() {
    let parsed = parse_module_with_file(
        "pub fn legacy<T: Display>(value: T): T = value;",
        "legacy-bound.orna",
    );
    assert!(parsed.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "ORNA091-E-BOUND-COLON"
            && diagnostic.message == "protocol bounds use `<T impl Protocol>`"
    }));
}

#[test]
fn generic_calls_retain_callee_effect_and_failure_summaries() {
    let filesystem = analyze_main(
        r#"
            pub protocol Display {
                fn display(self): Str;
            }
            pub type Note {
                value: Str,
                impl Display {
                    fn display(self): Str = self.value;
                }
            }
            pub fn reads<T impl Display>(value: T): Bool =
                std.io.fs.read_text("private-input") == "ok";
            pub table Book(id: Int) { value: Str, }
            pub table Loan(id: Int) { book_id: Int, }
            assert every(Loan, loan =>
                exists(Book, book => book.id == loan.book_id)
                && reads<Note>(Note { value: "note" })
            );
        "#,
    );
    assert!(filesystem.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "declaration assertion uses forbidden filesystem effect"
    }));
    let reads = filesystem
        .modules
        .values()
        .next()
        .expect("filesystem module")
        .symbols
        .get("reads")
        .expect("generic filesystem function");
    assert!(reads.effects.effects.contains("filesystem"));
    assert!(reads.effects.may_fail);

    let fallible = analyze_main(
        r#"
            pub protocol Display {
                fn display(self): Str;
            }
            pub type Note {
                value: Str,
                impl Display {
                    fn display(self): Str = self.value;
                }
            }
            pub fn maybe<T impl Display>(value: T): Bool = one([true]);
            pub table Book(id: Int) { value: Str, }
            pub table Loan(id: Int) { book_id: Int, }
            assert every(Loan, loan =>
                exists(Book, book => book.id == loan.book_id)
                && maybe<Note>(Note { value: "note" })
            );
        "#,
    );
    assert!(fallible.diagnostics.iter().any(|diagnostic| {
        diagnostic.message() == "assertion has forbidden effects or failure"
    }));
    let maybe = fallible
        .modules
        .values()
        .next()
        .expect("fallible module")
        .symbols
        .get("maybe")
        .expect("generic fallible function");
    assert!(maybe.effects.may_fail);
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
