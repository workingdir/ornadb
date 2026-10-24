use orna_semantic_v1::{Analysis, DIAG_TYPE, DIAG_UNRESOLVED, ModuleInput, analyze};

const SECRET_STORED_MESSAGE: &str = "secret values cannot be stored in table fields";

fn analysis_for(source: &str) -> Analysis {
    analyze(&[ModuleInput::new("accounts/main.orna", source)])
}

fn table_source(field: &str) -> String {
    format!("pub table Account(id: Uuid) {{\n    {field},\n}}\n")
}

#[test]
// Specified: 29-formats.md:115 serialisable subset; a stored field without an
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
// Specified: ORNA-SECRET-001 and 29-formats.md:115; a nominal record whose member is a secret is
// a stored secret, so a stored field of that nominal is rejected like a bare `Secret<Str>`.
fn stored_field_rejects_nominal_record_containing_secret() {
    let source = "pub type Credential {\n    token: Secret<Str>,\n}\npub table Account(id: Uuid) {\n    cred: Credential,\n}\n";
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
// Specified: ORNA-SECRET-001 and 29-formats.md:115; a transparent alias of a nominal that holds a
// secret stores that secret.
fn stored_field_rejects_alias_of_nominal_containing_secret() {
    let source = "pub type Credential {\n    token: Secret<Str>,\n}\ntype Login = Credential;\npub table Account(id: Uuid) {\n    login: Login,\n}\n";
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
// Specified: ORNA-SECRET-001; a self-referential nominal must not loop while it is checked, and
// its secret member is still found.
fn stored_field_rejects_recursive_nominal_containing_secret() {
    let source = "pub type Node {\n    next: List<Node>,\n    token: Secret<Str>,\n}\npub table Account(id: Uuid) {\n    node: Node,\n}\n";
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

fn assert_rejects_secret(analysis: &Analysis) {
    assert!(!analysis.is_ok(), "{:#?}", analysis.diagnostics);
    assert!(
        analysis.diagnostics.iter().any(|diagnostic| {
            diagnostic.code() == DIAG_TYPE && diagnostic.message() == SECRET_STORED_MESSAGE
        }),
        "{:#?}",
        analysis.diagnostics
    );
}

#[test]
// Specified: ORNA-SECRET-001 and 29-formats.md:115; an alias inside a nominal row resolves to its
// target, so a nominal whose member is an alias of `Secret` stores that secret.
fn stored_field_rejects_nominal_member_alias_of_secret() {
    let analysis = analysis_for(
        "type Token = Secret<Str>;\npub type Cred {\n    t: Token,\n}\npub table Account(id: Uuid) {\n    cred: Cred,\n}\n",
    );
    assert_rejects_secret(&analysis);
}

#[test]
// Specified: ORNA-SECRET-001 and 29-formats.md:115; an alias of a nominal that is itself a member
// of another nominal still reaches the secret.
fn stored_field_rejects_nominal_member_alias_of_nominal_containing_secret() {
    let analysis = analysis_for(
        "pub type Credential {\n    token: Secret<Str>,\n}\ntype Login = Credential;\npub type Session {\n    login: Login,\n}\npub table Account(id: Uuid) {\n    session: Session,\n}\n",
    );
    assert_rejects_secret(&analysis);
}

#[test]
// Specified: ORNA-SECRET-001; a secret nested through a nominal member of a nominal is stored.
fn stored_field_rejects_nested_nominal_containing_secret() {
    let analysis = analysis_for(
        "pub type Inner {\n    token: Secret<Str>,\n}\npub type Cred {\n    inner: Inner,\n}\npub table Account(id: Uuid) {\n    cred: Cred,\n}\n",
    );
    assert_rejects_secret(&analysis);
}

#[test]
// Specified: 29-formats serialisable subset; a nominal without any secret is a valid stored field.
fn stored_field_accepts_nominal_without_secret() {
    let analysis = analysis_for(
        "pub type Addr {\n    street: Str,\n    tags: List<Str>,\n}\npub table Account(id: Uuid) {\n    addr: Addr,\n}\n",
    );
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
}

#[test]
// Specified: ORNA-SECRET-001; an imported nominal whose exported (public) member is a
// secret is a stored secret. Private members are not exported (see declared_nominal_schema).
fn stored_field_rejects_imported_nominal_containing_secret() {
    let analysis = analyze(&[
        ModuleInput::new(
            "main.orna",
            "use creds.{Credential};\n\npub table Account(id: Uuid) {\n    cred: Credential,\n}\n",
        ),
        ModuleInput::new("creds.orna", "pub type Credential {\n    pub token: Secret<Str>,\n}\n"),
    ]);
    assert_rejects_secret(&analysis);
}

#[test]
// Specified: 29-formats serialisable subset; ordinary stored fields keep passing.
fn ordinary_stored_fields_remain_valid() {
    let analysis = analysis_for(
        "pub table Account(id: Uuid) {\n    name: Str,\n    note: Str?,\n    tags: List<Str>,\n    count: Int = 0,\n}\n",
    );
    assert!(analysis.is_ok(), "{:#?}", analysis.diagnostics);
}
