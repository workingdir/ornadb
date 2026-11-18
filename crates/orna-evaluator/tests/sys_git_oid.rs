//! `sys.GitOid` as an evaluator value: a Git object id that compares by the object it names, orders by algorithm
//! and then bytes, survives the REPL's `$_` binding and is never a `Str`, a `Uuid` or a digest (ORNA-SYS-016).
//! Its canonical form is the tag-60023 value `["sys.GitOid", ["sha1", bytes20]]` or
//! `["sys.GitOid", ["sha256", bytes32]]` (`source/29-formats.md`).

use orna_evaluator::{Environment, Functions, Limits, ReplSession, evaluate_expression};
use orna_value::{CanonicalValue, GitObjectId, OPAQUE_SYSTEM_ID_TAG, OvbRaw};

const LOW: &str = "0000000000000000000000000000000000000001";
const MIDDLE: &str = "3f786850e387550fdab836ed7e6dc881de23001b";
const HIGH: &str = "ffffffffffffffffffffffffffffffffffffffff";

fn oid(hex: &str) -> CanonicalValue {
    GitObjectId::from_hex(hex)
        .expect("an object id")
        .to_canonical()
        .expect("a canonical sys.GitOid")
}

fn sha256() -> CanonicalValue {
    oid(&"00".repeat(32))
}

fn tagged(tag: u64, body: OvbRaw) -> CanonicalValue {
    CanonicalValue::new(OvbRaw::Tag(tag, Box::new(body))).expect("a canonical tagged value")
}

fn environment() -> Environment {
    Environment::from([
        ("low".to_owned(), oid(LOW)),
        ("middle".to_owned(), oid(MIDDLE)),
        ("high".to_owned(), oid(HIGH)),
        ("sha256".to_owned(), sha256()),
        // The same 32 bytes as `sha256`, as a digest.
        (
            "digest".to_owned(),
            tagged(
                60025,
                OvbRaw::Array(vec![
                    OvbRaw::Text("sha256".to_owned()),
                    OvbRaw::Bytes(vec![0; 32]),
                ]),
            ),
        ),
    ])
}

/// An environment of one tag-60023 identifier that is not a Git object id.
fn revision_environment() -> Environment {
    Environment::from([(
        "revision".to_owned(),
        tagged(
            OPAQUE_SYSTEM_ID_TAG,
            OvbRaw::Array(vec![
                OvbRaw::Text("sys.RevisionId".to_owned()),
                OvbRaw::Bytes(vec![9; 32]),
            ]),
        ),
    )])
}

fn evaluate(source: &str) -> CanonicalValue {
    evaluate_expression(source, &environment(), Limits::default())
        .unwrap_or_else(|error| panic!("{source}: {}", error.code()))
}

fn boolean(value: bool) -> CanonicalValue {
    CanonicalValue::new(OvbRaw::Bool(value)).expect("a Bool is canonical")
}

fn list(values: &[CanonicalValue]) -> CanonicalValue {
    CanonicalValue::new(OvbRaw::Array(
        values.iter().map(|value| value.raw().clone()).collect(),
    ))
    .expect("a list is canonical")
}

fn session() -> ReplSession {
    ReplSession::with_bindings(Limits::default(), environment(), Functions::new())
        .expect("an admitted session")
}

// Specified: ORNA-SYS-016, ORNA-FORMAT-001; Orna-1.1.0 29-formats.md, tag 60023 and `sys.GitOid`.
#[test]
fn a_git_oid_binding_reads_back_as_the_same_canonical_value() {
    for (name, expected) in [
        ("low", oid(LOW)),
        ("middle", oid(MIDDLE)),
        ("high", oid(HIGH)),
        ("sha256", sha256()),
    ] {
        assert_eq!(evaluate(name), expected, "{name}");
    }
    // A SHA-1 id and a SHA-256 id are distinct values with their own algorithm name.
    assert_ne!(oid(LOW), sha256());
    let OvbRaw::Tag(OPAQUE_SYSTEM_ID_TAG, body) = oid(MIDDLE).raw().clone() else {
        panic!("a sys.GitOid is tag 60023");
    };
    let OvbRaw::Array(parts) = *body else {
        panic!("a sys.GitOid is [type name, representation]");
    };
    assert_eq!(parts[0], OvbRaw::Text("sys.GitOid".to_owned()));
}

// Specified: ORNA-SYS-016; ids compare by the identity they name and are not interchangeable with
// another type.
#[test]
fn ids_compare_by_the_object_they_name_and_never_as_another_type() {
    assert_eq!(evaluate("middle == middle"), boolean(true));
    assert_eq!(evaluate("middle == low"), boolean(false));
    assert_eq!(evaluate("middle != low"), boolean(true));
    assert_eq!(evaluate("middle != middle"), boolean(false));
    // Neither the text spelling of an id, nor a digest of the same bytes, nor another opaque identifier is the id.
    assert_eq!(evaluate(&format!("middle == \"{MIDDLE}\"")), boolean(false));
    assert_eq!(evaluate("sha256 == digest"), boolean(false));
    // Ids are values: they compare inside collections and records.
    assert_eq!(evaluate("[low, middle] == [low, middle]"), boolean(true));
    assert_eq!(evaluate("[low, middle] == [low, high]"), boolean(false));
    assert_eq!(evaluate("{ id: low } == { id: low }"), boolean(true));
    assert_eq!(evaluate("{ id: low } == { id: middle }"), boolean(false));
}

// Specified: ORNA-SYS-016; ids order by algorithm and then bytes, which for one algorithm is the order
// of Git's own lowercase hexadecimal spellings.
#[test]
fn ids_order_by_algorithm_and_then_bytes() {
    assert_eq!(evaluate("low < middle"), boolean(true));
    assert_eq!(evaluate("middle < high"), boolean(true));
    assert_eq!(evaluate("high < middle"), boolean(false));
    assert_eq!(evaluate("high > middle"), boolean(true));
    assert_eq!(evaluate("middle <= middle"), boolean(true));
    assert_eq!(evaluate("middle >= high"), boolean(false));
    // Every SHA-1 id sorts before every SHA-256 id.
    assert_eq!(evaluate("high < sha256"), boolean(true));
    assert_eq!(
        evaluate("sort_by([high, low, middle, sha256], id => id)"),
        list(&[oid(LOW), oid(MIDDLE), oid(HIGH), sha256()])
    );
    // Ordering belongs to ids alone: an id and a digest do not compare.
    let failure = evaluate_expression("sha256 < digest", &environment(), Limits::default())
        .expect_err("an id and a digest do not order");
    assert_eq!(failure.code(), "ORNA-EVAL-TYPE");
}

// Specified: ORNA-SYS-016, ORNA-FORMAT-001; an id nested in a collection, a record or a tuple has the
// same canonical form as the id alone.
#[test]
fn ids_nest_in_lists_records_and_tuples() {
    assert_eq!(evaluate("[low, high]"), list(&[oid(LOW), oid(HIGH)]));
    assert_eq!(
        evaluate("{ id: middle }"),
        CanonicalValue::new(OvbRaw::Map(vec![(
            OvbRaw::Text("id".to_owned()),
            oid(MIDDLE).raw().clone(),
        )]))
        .expect("a record is canonical")
    );
    assert_eq!(
        evaluate("(low, high)"),
        tagged(
            60015,
            OvbRaw::Array(vec![oid(LOW).raw().clone(), oid(HIGH).raw().clone()])
        )
    );
}

// Specified: ORNA-REPL-002; `$_` is the last successful REPL result. The session decodes every `$_`
// binding for the next input, so a result the evaluator could not read back would break it.
#[test]
fn the_repl_last_result_survives_a_git_oid_result() {
    let mut session = session();
    assert_eq!(
        session.submit("middle").expect("a Git object id result"),
        Some(oid(MIDDLE))
    );
    assert_eq!(
        session.submit("$_").expect("`$_` holds the id"),
        Some(oid(MIDDLE))
    );
    assert_eq!(
        session.submit("$_ == middle").expect("`$_` is the id"),
        Some(boolean(true))
    );
}

// Specified: ORNA-REPL-002; every input decodes the session's `$_`, so an id result must not break an
// input that never mentions it.
#[test]
fn an_input_that_ignores_the_last_result_still_runs_after_a_git_oid_result() {
    let mut session = session();
    session.submit("middle").expect("a Git object id result");
    assert_eq!(
        session
            .submit("1 + 1")
            .expect("an input that never mentions `$_`"),
        Some(CanonicalValue::new(OvbRaw::Int(2.into())).expect("an Int is canonical"))
    );
}

// Specified: ORNA-REPL-002; ids nested in a result survive `$_` too.
#[test]
fn the_repl_last_result_survives_a_result_that_holds_ids() {
    let mut session = session();
    for (source, expected) in [
        ("[low, high]", list(&[oid(LOW), oid(HIGH)])),
        (
            "(low, high)",
            tagged(
                60015,
                OvbRaw::Array(vec![oid(LOW).raw().clone(), oid(HIGH).raw().clone()]),
            ),
        ),
        (
            "{ id: middle }",
            CanonicalValue::new(OvbRaw::Map(vec![(
                OvbRaw::Text("id".to_owned()),
                oid(MIDDLE).raw().clone(),
            )]))
            .expect("a record is canonical"),
        ),
    ] {
        assert_eq!(
            session.submit(source).expect("a result holding ids"),
            Some(expected.clone()),
            "{source}"
        );
        assert_eq!(
            session.submit("$_").expect("`$_` holds the result"),
            Some(expected),
            "`$_` after {source}"
        );
    }
}

// Specified: ORNA-REPL-001; a session binding keeps an id as the same value for later inputs.
#[test]
fn a_session_binding_keeps_its_id() {
    let mut session = session();
    assert_eq!(session.submit("let tip = high;").expect("a binding"), None);
    assert_eq!(
        session.submit("tip").expect("the bound id"),
        Some(oid(HIGH))
    );
    assert_eq!(
        session.submit("tip > middle").expect("a comparison"),
        Some(boolean(true))
    );
}

// Specified: ORNA-SYS-016; another opaque identifier is tag 60023 too and is not read as a Git object
// id.
#[test]
fn an_opaque_identifier_that_is_not_a_git_oid_is_not_read_as_one() {
    let failure = evaluate_expression("revision", &revision_environment(), Limits::default())
        .expect_err("a sys.RevisionId is not a Git object id");
    assert_eq!(failure.code(), "ORNA-EVAL-UNSUPPORTED");
}
