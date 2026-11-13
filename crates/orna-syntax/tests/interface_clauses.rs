//! NATIVE-2 (sys-as-native SN1): native types, native statics, doc comments and interface clauses.
//!
//! Each form and clause is accepted only where the spec's clause table names it, only in an
//! engine-embedded module, and never becomes a keyword.

use orna_syntax::{Parse, TokenKind, lex, parse_interface, parse_module};

use orna_syntax_ast::{Declaration, SyntaxTree};

const CLAUSE_DIAGNOSTIC: &str = "ORNA091-E-INTERFACE-CLAUSE";
const PLACE_DIAGNOSTIC: &str = "ORNA091-E-NATIVE-PLACE";

fn codes(parsed: &Parse<SyntaxTree>) -> Vec<String> {
    parsed
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.code.to_string())
        .collect()
}

#[test]
fn native_type_and_static_are_accepted_in_engine_modules() {
    let parsed = parse_interface(
        "pub native type Snapshot<T>;\npub native static runtime: Runtime effect none;",
    );
    assert!(parsed.is_ok(), "{:?}", codes(&parsed));

    let items = &parsed.value.items;
    match &items[0].declaration {
        Declaration::NativeType { name, generics, .. } => {
            assert_eq!(name, "Snapshot");
            assert_eq!(generics.len(), 1);
        }
        other => panic!("expected a native type, got {other:?}"),
    }
    match &items[1].declaration {
        Declaration::NativeStatic {
            name,
            annotation,
            effects,
            ..
        } => {
            assert_eq!(name, "runtime");
            assert!(annotation.is_some());
            assert!(effects.is_some());
        }
        other => panic!("expected a native static, got {other:?}"),
    }
}

#[test]
fn native_type_and_static_are_rejected_in_user_modules() {
    for source in [
        "pub native type Snapshot<T>;",
        "pub native static runtime: Runtime effect none;",
    ] {
        let parsed = parse_module(source);
        assert!(
            codes(&parsed).iter().any(|code| code == PLACE_DIAGNOSTIC),
            "{source} must report {PLACE_DIAGNOSTIC}, got {:?}",
            codes(&parsed)
        );
    }
}

#[test]
fn interface_clauses_are_accepted_on_their_forms_and_kept() {
    let source = "\
pub native fn probe(id: Str): Str effect none class read since \"1.1.0\" fails object__not_found, snapshot__expired spelling \"sys.probe(Str)\";
pub native fn gone(): Int effect none removed \"1.1.0\" use \"sys.new\" reason \"moved\";
pub native fn old(): Int effect none formerly \"sys.runtime\" diagnostic \"code\" deprecated \"1.2.0\" use \"sys.new\";
pub native static rt: Runtime effect none formerly \"sys.runtime\";
pub native type Old removed \"1.1.0\";
pub type Object key id, name availability \"catalogue\" { id: Int, name: Str, }
";
    let parsed = parse_interface(source);
    assert!(parsed.is_ok(), "{:?}", codes(&parsed));

    let items = &parsed.value.items;
    let probe = &items[0].clauses;
    assert_eq!(
        probe.class.as_ref().map(|class| class.text.as_str()),
        Some("read")
    );
    assert_eq!(
        probe.since.as_ref().map(|since| since.text.as_str()),
        Some("1.1.0")
    );
    let fails: Vec<&str> = probe.fails.iter().map(|code| code.text.as_str()).collect();
    assert_eq!(fails, vec!["object__not_found", "snapshot__expired"]);
    assert_eq!(
        probe
            .spelling
            .as_ref()
            .map(|spelling| spelling.text.as_str()),
        Some("sys.probe(Str)")
    );

    let gone = items[1].clauses.removed.as_ref().expect("removed clause");
    assert_eq!(gone.version.text, "1.1.0");
    assert_eq!(
        gone.replacement.as_ref().map(|p| p.text.as_str()),
        Some("sys.new")
    );
    assert_eq!(gone.reason.as_ref().map(|r| r.text.as_str()), Some("moved"));

    let old = &items[2].clauses;
    assert_eq!(
        old.formerly.as_ref().map(|f| f.name.text.as_str()),
        Some("sys.runtime")
    );
    assert_eq!(
        old.deprecated.as_ref().map(|d| d.version.text.as_str()),
        Some("1.2.0")
    );

    assert!(items[3].clauses.formerly.is_some());
    assert!(items[4].clauses.removed.is_some());

    let object = &items[5].clauses;
    let keys: Vec<&str> = object.key.iter().map(|key| key.text.as_str()).collect();
    assert_eq!(keys, vec!["id", "name"]);
    assert_eq!(
        object.availability.as_ref().map(|a| a.text.as_str()),
        Some("catalogue")
    );
}

#[test]
fn clauses_are_rejected_in_user_modules() {
    for source in [
        "pub native fn f(): Int effect none since \"1.1.0\";",
        "pub type Row key id { id: Int, }",
    ] {
        let parsed = parse_module(source);
        assert!(
            codes(&parsed).iter().any(|code| code == CLAUSE_DIAGNOSTIC),
            "{source} must report {CLAUSE_DIAGNOSTIC}, got {:?}",
            codes(&parsed)
        );
    }
}

#[test]
fn clauses_are_rejected_on_forms_their_table_does_not_name() {
    for source in [
        "pub native type Old since \"1.1.0\";",
        "pub native fn f(): Int effect none key id;",
        "pub native static s: Int effect none spelling \"x\";",
        "pub type Row class read { id: Int, }",
    ] {
        let parsed = parse_interface(source);
        assert!(
            codes(&parsed).iter().any(|code| code == CLAUSE_DIAGNOSTIC),
            "{source} must report {CLAUSE_DIAGNOSTIC}, got {:?}",
            codes(&parsed)
        );
    }
}

#[test]
fn clause_words_are_not_keywords() {
    // ORNA-NATIVE-019 and ORNA-LEX-007: every clause word lexes as an identifier.
    let words = "class fails since removed formerly deprecated spelling key availability";
    let tokens = lex(words).expect("the clause words lex");
    for token in &tokens[..9] {
        assert!(
            matches!(&token.kind, TokenKind::Identifier { .. }),
            "{} must stay an identifier",
            token.text
        );
    }

    // Each remains a valid name in a user module.
    let parsed = parse_module(
        "fn since(key: Int, availability: Int, fails: Int, class: Int): Int = key + availability;",
    );
    assert!(parsed.is_ok(), "{:?}", codes(&parsed));
}

#[test]
fn doc_lines_attach_to_the_following_declaration() {
    let parsed =
        parse_module("/// First line.\n///\n/// Second.\npub fn add(a: Int, b: Int): Int = a + b;");
    assert!(parsed.is_ok(), "{:?}", codes(&parsed));
    assert_eq!(
        parsed.value.items[0].docs,
        vec!["First line.", "", "Second."]
    );
}

#[test]
fn four_slashes_are_an_ordinary_comment_not_a_doc_line() {
    let parsed = parse_module("//// not documentation\npub fn f(): Int = 1;");
    assert!(parsed.is_ok(), "{:?}", codes(&parsed));
    assert!(parsed.value.items[0].docs.is_empty());
}

#[test]
fn doc_lines_are_kept_on_engine_native_declarations() {
    let parsed =
        parse_interface("/// The runtime.\npub native static runtime: Runtime effect none;");
    assert!(parsed.is_ok(), "{:?}", codes(&parsed));
    assert_eq!(parsed.value.items[0].docs, vec!["The runtime."]);
}
