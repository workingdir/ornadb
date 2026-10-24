//! Regression tests for semantic clause gaps (ORNA-NOMINAL-008,
//! ORNA-MEMBER-003). Each test pins the behaviour the spec requires and checks
//! diagnostic codes rather than message wording.

use orna_foundation_v1::Diagnostic;
use orna_semantic_v1::{DIAG_UNRESOLVED, DIAG_UNSUPPORTED, ModuleInput, analyze};

fn diagnostics_of(source: &str) -> Vec<Diagnostic> {
    analyze(&[ModuleInput::new("main.orna", source)]).diagnostics
}

// Specified: ORNA-NOMINAL-008, source/05-types.md:380. Nominal values remain
// immutable, so assigning to a field of a nominal value is rejected.
#[test]
fn nominal_field_assignment_is_rejected_as_unsupported() {
    let diagnostics = diagnostics_of(
        "pub type SecretToken {
    value: Str,
}

pub fn rewrite(): Str {
    let token = SecretToken { value: \"a\" };
    token.value = \"b\";
    token.value
}
",
    );

    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_UNSUPPORTED),
        "{diagnostics:#?}"
    );
}

// Specified: ORNA-MEMBER-003, source/06-expressions.md:368. A free function
// whose first parameter matches the receiver MUST NOT become a dot method:
// `1.double()` must not resolve to `double(1)`.
#[test]
fn free_function_is_not_callable_with_method_syntax() {
    let diagnostics = diagnostics_of(
        "pub fn double(value: Int): Int = value * 2;

pub fn sum(): Int = 1.double();
",
    );

    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].code(), DIAG_UNRESOLVED, "{diagnostics:#?}");
}
