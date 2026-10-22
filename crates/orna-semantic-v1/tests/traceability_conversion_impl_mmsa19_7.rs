//! Focused traceability evidence for conversion dispatch and nested
//! implementation ownership. These tests run the retained semantic analyzer on
//! inline Orna sources; they do not claim runtime conversion execution.

use orna_semantic_v1::{
    Analysis, Catalogue, DIAG_CONVERSION_CHAIN, DIAG_LEGACY_TRYFROM, DIAG_TYPE, ModuleInput, Symbol,
    Type, analyze, analyze_with_catalogue,
};

const CHAIN_DIAGNOSTIC: &str =
    "implicit conversion chains are not searched; name each conversion explicitly";
const PROTOCOL_DIAGNOSTIC: &str = "generic type argument does not satisfy its protocol bound";

fn export<'a>(result: &'a Analysis, name: &str) -> &'a Symbol {
    result
        .modules
        .values()
        .find_map(|module| module.exports.get(name))
        .unwrap_or_else(|| panic!("no export {name}: {:#?}", result.diagnostics))
}

fn returns_nominal(ty: &Type, suffix: &str) -> bool {
    match ty {
        Type::Function { result, .. } => matches!(
            result.as_ref(),
            Type::Named(name) if name == suffix || name.ends_with(&format!(".{suffix}"))
        ),
        _ => false,
    }
}

fn count_protocol_diagnostics(result: &Analysis) -> usize {
    result
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.message() == PROTOCOL_DIAGNOSTIC)
        .count()
}

// Specified: ORNA-CONVERT-002, source/05-types.md:434.
// Exists: a nominal target with two From implementations for distinct sources.
// Passed is bounded to both source-typed Target.from calls analyzing as Target.
#[test]
fn nominal_type_accepts_from_implementations_for_distinct_sources() {
    let result = analyze(&[ModuleInput::new(
        "multi-from.orna",
        r#"
pub type Target {
    value: Str,
    impl From<Str> { fn from(text) = Target { value: text }; }
    impl From<Int> { fn from(number) = Target { value: "number" }; }
}
pub fn from_text(text: Str): Target = Target.from(text);
pub fn from_number(number: Int): Target = Target.from(number);
"#,
    )]);

    assert!(result.is_ok(), "{:#?}", result.diagnostics);
    for name in ["from_text", "from_number"] {
        assert!(
            returns_nominal(&export(&result, name).ty, "Target"),
            "{name}: {:?}",
            export(&result, name).ty
        );
    }
}

// Not cited: the failure-propagation clause lacks a discriminating test; tracked in ornadb-o7qg.
// Checks only that a From body using the ordinary fail channel is accepted.
// Does not check that the failure reaches the call site.
#[test]
fn from_body_using_ordinary_failure_channel_is_accepted() {
    let result = analyze_with_catalogue(
        &[ModuleInput::new(
            "fallible-from.orna",
            r#"
pub type Target {
    value: Str,
    impl From<Str> {
        fn from(value): Target {
            if value == "" {
                fail(error(code: "target.empty", message: "empty target"));
            }
            Target { value: value }
        }
    }
}
pub fn parse(text: Str): Target = Target.from(text);
"#,
        )],
        &Catalogue::authoritative_fixture(),
    );

    assert!(result.is_ok(), "{:#?}", result.diagnostics);
}

// Specified: ORNA-CONVERT-005, source/05-types.md:440.
// Exists: a legacy TryFrom implementation spelling.
// Passed is bounded to the legacy diagnostic proposing From<Source> instead.
#[test]
fn legacy_try_from_spelling_proposes_from() {
    let result = analyze(&[ModuleInput::new(
        "try-from.orna",
        "pub type Port { impl TryFrom<Int> { fn from(value) = Port { value: value }; } }",
    )]);

    assert!(
        result.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DIAG_LEGACY_TRYFROM
                && diagnostic.message() == "use From<Source>; From may fail in Orna"
        }),
        "{:#?}",
        result.diagnostics
    );
}

// Specified: ORNA-CONVERT-007, source/05-types.md:444.
// Exists: a two-step From chain (Raw -> Email -> Account).
// Passed is bounded to rejecting the implicit one-expression chain while
// accepting the same chain when each conversion is named explicitly.
#[test]
fn implicit_multi_step_conversion_chain_is_not_executed() {
    let chain = r#"
pub type Raw { value: Str, }
pub type Email { value: Str, impl From<Raw> { fn from(raw) = Email { value: "x" }; } }
pub type Account { id: Str, impl From<Email> { fn from(email) = Account { id: "a" }; } }
"#;

    let implicit = analyze(&[ModuleInput::new(
        "chain-implicit.orna",
        format!("{chain}pub fn direct(raw: Raw): Account = raw;\n"),
    )]);
    assert!(
        implicit.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DIAG_CONVERSION_CHAIN && diagnostic.message() == CHAIN_DIAGNOSTIC
        }),
        "{:#?}",
        implicit.diagnostics
    );

    let explicit = analyze(&[ModuleInput::new(
        "chain-explicit.orna",
        format!("{chain}pub fn named(raw: Raw): Account = Account.from(Email.from(raw));\n"),
    )]);
    assert!(explicit.is_ok(), "{:#?}", explicit.diagnostics);
}

// Specified: ORNA-CONVERT-009, source/05-types.md:448.
// Exists: a private-field record construction inside the target's own From
// implementation, with the same construction rejected outside the owner.
// Passed is bounded to accepting the owner-lexical construction and rejecting
// the identical construction at module scope.
#[test]
fn conversion_implementation_accesses_private_fields_of_its_target() {
    let owned = analyze(&[ModuleInput::new(
        "owned-from.orna",
        r#"
pub type Token {
    value: Str,
    impl From<Str> { fn from(text) = Token { value: text }; }
}
"#,
    )]);
    assert!(owned.is_ok(), "{:#?}", owned.diagnostics);

    let outside = analyze(&[ModuleInput::new(
        "outside-owner.orna",
        r#"
pub type Token {
    value: Str,
    impl From<Str> { fn from(text) = Token { value: text }; }
}
pub fn make(text: Str): Token = Token { value: text };
"#,
    )]);
    assert!(
        outside
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_TYPE),
        "{:#?}",
        outside.diagnostics
    );
}

// Specified: ORNA-IMPL-002, source/05-types.md:489.
// Exists: a nested impl Display inside Username and a sibling type without one.
// Passed is bounded to the nested protocol being attributed to the enclosing
// nominal type for generic bound satisfaction.
#[test]
fn nested_impl_attaches_protocol_to_enclosing_nominal_type() {
    let result = analyze(&[ModuleInput::new(
        "nested-impl.orna",
        r#"
pub protocol Display { fn display(self): Str; }
pub type Username {
    value: Str,
    impl Display { fn display(self): Str = self.value; }
}
pub type Plain { value: Str, }
pub fn keep<T impl Display>(value: T): T = value;
pub fn accepted(): Username = keep(Username { value: "a" });
pub fn rejected(): Plain = keep(Plain { value: "a" });
"#,
    )]);

    assert_eq!(count_protocol_diagnostics(&result), 1, "{:#?}", result.diagnostics);
    assert!(
        returns_nominal(&export(&result, "accepted").ty, "Username"),
        "{:?}",
        export(&result, "accepted").ty
    );
}

// Specified: ORNA-IMPL-004, source/05-types.md:493.
// Exists: a refined nominal type with assertions and a nested impl Display in
// one body, and a sibling refined type with assertions but no nested impl.
// Passed is bounded to the refined type's nested protocol satisfying a generic
// bound alongside its assertions.
#[test]
fn refined_nominal_type_contains_nested_impl_with_assertions() {
    let result = analyze(&[ModuleInput::new(
        "refined-impl.orna",
        r#"
pub protocol Display { fn display(self): Str; }
pub type Port = Int {
    assert >= 1;
    assert <= 65_535;
    impl Display { fn display(self): Str = "port"; }
}
pub type Bare = Int {
    assert >= 1;
}
pub fn keep<T impl Display>(value: T): T = value;
pub fn accepted(): Port = keep(Port.from(8080));
pub fn rejected(): Bare = keep(Bare.from(8080));
"#,
    )]);

    assert_eq!(count_protocol_diagnostics(&result), 1, "{:#?}", result.diagnostics);
}
