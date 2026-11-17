//! `secret_access` is a host effect kind (`sys.EffectKind`). Every value
//! reached through `std.secret` carries it by resolved identity, so the purity
//! bans that reject host effects reject secret reads as well:
//!
//! - ORNA-FIELD-008: a computed field MUST NOT access secrets.
//! - ORNA-ASSERT-011 / ORNA-ASSERT-039: a table assertion MUST NOT use a
//!   forbidden effect, reported as ORNA-A091-007.

use orna_check::{
    Analysis, Catalogue, DIAG_ASSERTION_EFFECT, DIAG_COMPUTED_NONDETERMINISTIC, ModuleInput,
    analyze_with_catalogue,
};

fn analyze(source: &str) -> Analysis {
    analyze_with_catalogue(
        &[ModuleInput::new("consumer.orna", source)],
        &Catalogue::authoritative_core(),
    )
}

fn count(analysis: &Analysis, code: &str) -> usize {
    analysis
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code() == code)
        .count()
}

fn diagnostic_lines(analysis: &Analysis) -> String {
    analysis
        .diagnostics
        .iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.code(), diagnostic.message()))
        .collect::<Vec<_>>()
        .join("\n")
}

const SECRET_READ: &str = r#"std.secret.open(std.secret.ref("fixture.credential"), as: Str)"#;

#[test]
fn computed_field_reading_a_secret_is_rejected() {
    let analysis = analyze(&format!(
        "pub table Account(id: Str) {{\n    label: Str,\n    token: Str => {SECRET_READ},\n}}\n"
    ));

    assert_eq!(
        count(&analysis, DIAG_COMPUTED_NONDETERMINISTIC),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn computed_field_reading_a_secret_through_root_alias_is_rejected() {
    let analysis = analyze(
        "use std as core;\npub table Account(id: Str) {\n    label: Str,\n    token: Str => core.secret.open(core.secret.ref(\"fixture.credential\"), as: Str),\n}\n",
    );

    assert_eq!(
        count(&analysis, DIAG_COMPUTED_NONDETERMINISTIC),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn computed_field_reading_a_secret_through_named_import_is_rejected() {
    let analysis = analyze(
        "use std.secret.{open, ref};\npub table Account(id: Str) {\n    label: Str,\n    token: Str => open(ref(\"fixture.credential\"), as: Str),\n}\n",
    );

    assert_eq!(
        count(&analysis, DIAG_COMPUTED_NONDETERMINISTIC),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn table_assertion_reading_a_secret_is_rejected() {
    let analysis = analyze(&format!(
        "pub table Account(id: Str) {{\n    label: Str,\n    assert {SECRET_READ} == \"x\";\n}}\n"
    ));

    assert_eq!(
        count(&analysis, DIAG_ASSERTION_EFFECT),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn table_assertion_reading_a_secret_through_root_alias_is_rejected() {
    let analysis = analyze(
        "use std as core;\npub table Account(id: Str) {\n    label: Str,\n    assert core.secret.open(core.secret.ref(\"fixture.credential\"), as: Str) == \"x\";\n}\n",
    );

    assert_eq!(
        count(&analysis, DIAG_ASSERTION_EFFECT),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}

#[test]
fn table_assertion_reading_a_secret_through_named_import_is_rejected() {
    let analysis = analyze(
        "use std.secret.{open, ref};\npub table Account(id: Str) {\n    label: Str,\n    assert open(ref(\"fixture.credential\"), as: Str) == \"x\";\n}\n",
    );

    assert_eq!(
        count(&analysis, DIAG_ASSERTION_EFFECT),
        1,
        "{}",
        diagnostic_lines(&analysis)
    );
}
