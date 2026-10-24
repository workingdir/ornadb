use orna_semantic_v1::{Analysis, DIAG_TYPE, DIAG_UNRESOLVED, ModuleInput, analyze};

const SECRET_STORED_MESSAGE: &str = "secret values cannot be stored in table fields";

fn analysis_for(source: &str) -> Analysis {
    analyze(&[ModuleInput::new("accounts/main.orna", source)])
}

fn table_source(field: &str) -> String {
    format!("pub table Account(id: Uuid) {{\n    {field},\n}}\n")
}

#[test]
// Specified: ORNA-FIELD-010 and 29-formats serialisable subset; a stored field without an
// initialiser is type-validated like every other table field.
fn stored_field_without_initialiser_rejects_unknown_type() {
    let analysis = analysis_for(&table_source("token: Bogus<Str>"));
    assert!(!analysis.is_ok());
    assert!(
        analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == DIAG_UNRESOLVED),
        "{:#?}",
        analysis.diagnostics
    );
}

#[test]
// Specified: ORNA-SECRET-001 and 29-formats; `Secret<...>` is outside the serialisable subset.
fn stored_field_without_initialiser_rejects_secret_type() {
    let analysis = analysis_for(&table_source("token: Secret<Str>"));
    assert!(!analysis.is_ok());
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DIAG_TYPE && diagnostic.message() == SECRET_STORED_MESSAGE
        }),
        "{:#?}",
        analysis.diagnostics
    );
}

#[test]
// Specified: ORNA-SECRET-001 and 29-formats; a transparent alias of `Secret` stores a secret.
fn stored_field_rejects_alias_of_secret_type() {
    let source = "type Token = Secret<Str>;\npub table Account(id: Uuid) {\n    token: Token,\n}\n";
    let analysis = analysis_for(source);
    assert!(!analysis.is_ok());
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DIAG_TYPE && diagnostic.message() == SECRET_STORED_MESSAGE
        }),
        "{:#?}",
        analysis.diagnostics
    );
}

#[test]
// Specified: ORNA-SECRET-001; a secret nested inside a stored container is still stored.
fn stored_field_rejects_secret_nested_in_list() {
    let analysis = analysis_for(&table_source("tokens: List<Secret<Str>>"));
    assert!(!analysis.is_ok());
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DIAG_TYPE && diagnostic.message() == SECRET_STORED_MESSAGE
        }),
        "{:#?}",
        analysis.diagnostics
    );
}

#[test]
// Specified: 29-formats "Closed storage schema descriptor"; a live stream is not a stored value.
fn stored_field_rejects_live_stream_type() {
    let analysis = analysis_for(&table_source("events: Stream<Str>"));
    assert!(!analysis.is_ok());
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DIAG_TYPE
                && diagnostic.message() == "stored table field type must be serialisable"
        }),
        "{:#?}",
        analysis.diagnostics
    );
}

#[test]
// Specified: 29-formats serialisable subset; ordinary stored fields keep passing.
fn ordinary_stored_fields_remain_valid() {
    let analysis = analysis_for(
        "pub table Account(id: Uuid) {\n    name: Str,\n    note: Str?,\n    tags: List<Str>,\n    count: Int = 0,\n}\n",
    );
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
}
