//! U03-12 and U03-13 over the real JSON-RPC transport.
//!
//! U03-12: the parser keeps a copy of a `for` body among the loop's arms; a
//! call inside the loop is one call for code lenses, call hierarchy and
//! parameter hints. U03-13: a `let` type hint appears only when the source
//! states the type, never for `null`, arithmetic or an undeclared result.

#[path = "support/lsp_client.rs"]
mod lsp_client;

use lsp_client::{Client, position_of};
use serde_json::{Value, json};

const LOOP: &str = "pub fn step(value: Int): Int = value + 1;\n\
    pub fn total(values: [Int]): Int {\n    let sum = 0;\n    \
    for value in values {\n        sum = step(value);\n    }\n    sum\n}\n";

const LETS: &str = "pub fn add(left: Int, right: Int): Int = left + right;\n\
    pub fn untyped(value: Int) = value;\n\
    pub fn demo(): Int {\n    let nothing = null;\n    let sum = 1 + 2;\n    \
    let flag = !true;\n    let typed = add(1, 2);\n    let guessed = untyped(1);\n    typed\n}\n";

fn whole(uri: &str, source: &str) -> Value {
    json!({
        "textDocument": {"uri": uri},
        "range": {
            "start": {"line": 0, "character": 0},
            "end": {"line": source.lines().count(), "character": 0},
        },
    })
}

#[test]
fn a_call_inside_a_for_loop_is_one_call() {
    let uri = "file:///workspace/loop-calls.orna";
    let mut client = Client::start();
    client.open(uri, LOOP);
    assert!(client.diagnostics_at(uri, 1).is_empty());

    let lenses = client.result("textDocument/codeLens", json!({"textDocument": {"uri": uri}}));
    let step = lenses
        .as_array()
        .unwrap()
        .iter()
        .find(|lens| lens["range"]["start"]["line"] == 0)
        .expect("lens on step");
    assert_eq!(step["command"]["title"], "1 incoming calls");
    assert_eq!(step["command"]["arguments"][2].as_array().unwrap().len(), 1);

    let total = client.result(
        "textDocument/prepareCallHierarchy",
        json!({"textDocument": {"uri": uri}, "position": position_of(LOOP, "total", 1)}),
    );
    let outgoing = client.result("callHierarchy/outgoingCalls", json!({"item": total[0]}));
    assert_eq!(outgoing.as_array().unwrap().len(), 1, "{outgoing}");
    assert_eq!(outgoing[0]["fromRanges"].as_array().unwrap().len(), 1);

    let hints = client.result("textDocument/inlayHint", whole(uri, LOOP));
    let parameters = hints
        .as_array()
        .unwrap()
        .iter()
        .filter(|hint| hint["kind"] == 2)
        .count();
    assert_eq!(parameters, 1, "one parameter hint for the one call: {hints}");
    client.shutdown();
}

#[test]
fn type_hints_appear_only_where_the_source_states_the_type() {
    let uri = "file:///workspace/let-types.orna";
    let mut client = Client::start();
    client.open(uri, LETS);
    assert!(client.diagnostics_at(uri, 1).is_empty());
    let hints = client.result("textDocument/inlayHint", whole(uri, LETS));
    let types = hints
        .as_array()
        .unwrap()
        .iter()
        .filter(|hint| hint["kind"] == 1)
        .map(|hint| {
            (
                hint["label"].as_str().unwrap().to_owned(),
                hint["position"]["line"].as_u64().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        types,
        [(": Bool".to_owned(), 5), (": Int".to_owned(), 6)],
        "no hint for `null`, arithmetic or an undeclared result: {hints}"
    );
    client.shutdown();
}
