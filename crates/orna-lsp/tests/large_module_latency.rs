//! U03-11: requests on a 2,000-line module answer in bounded time, and a
//! slow request does not hold up the others.
//!
//! The module is the one the audit timed: a table, then 333 documented
//! function pairs whose second calls the first (2,001 lines). On the old
//! server, references took 140 s and full semantic tokens 178 s in a debug
//! build, because every request re-parsed and re-lexed per symbol. The bound
//! below is for an unoptimised test build on a loaded machine.

#[path = "support/lsp_client.rs"]
mod lsp_client;

use std::time::{Duration, Instant};

use lsp_client::{Client, position_of};
use serde_json::{Value, json};

const LIMIT: Duration = Duration::from_secs(5);

fn module() -> String {
    let mut source = String::from("pub table Item(id: Int) {\n    name: Str,\n}\n");
    for index in 0..333 {
        source.push_str(&format!(
            "/// Adds {index}.\npub fn f{index}(a: Int, b: Int): Int = a + b;\n\n\
             pub fn g{index}(row: Item): Int =\n    f{index}(row.id, {index});\n\n"
        ));
    }
    source
}

fn timed(client: &mut Client, method: &str, params: Value) -> (Duration, Value) {
    let started = Instant::now();
    let result = client.result(method, params);
    (started.elapsed(), result)
}

#[test]
fn a_two_thousand_line_module_answers_every_request_within_the_bound() {
    let source = module();
    assert_eq!(source.lines().count(), 2_001);
    let uri = "file:///workspace/large-module.orna";
    let mut client = Client::start();
    let opened = Instant::now();
    client.open(uri, &source);
    let diagnostics = client.diagnostics_at(uri, 1);
    let open_elapsed = opened.elapsed();
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let call = position_of(&source, "f166(row.id", 1);
    let declaration = position_of(&source, "fn f166(", "fn ".len());
    let at = |position: &Value| json!({"textDocument": {"uri": uri}, "position": position});
    let document = json!({"textDocument": {"uri": uri}});
    let lines = source.lines().count();
    let whole = json!({
        "textDocument": {"uri": uri},
        "range": {"start": {"line": 0, "character": 0}, "end": {"line": lines, "character": 0}},
    });
    let mut timings = vec![("didOpen -> publishDiagnostics", open_elapsed)];
    let mut check = |name: &'static str, (elapsed, result): (Duration, Value)| {
        timings.push((name, elapsed));
        result
    };

    let hover = check("hover", timed(&mut client, "textDocument/hover", at(&call)));
    assert!(hover["contents"]["value"].as_str().unwrap().contains("f166"), "{hover}");
    let definition = check("definition", timed(&mut client, "textDocument/definition", at(&call)));
    assert_eq!(definition["range"]["start"], declaration);
    let references = check(
        "references",
        timed(
            &mut client,
            "textDocument/references",
            json!({
                "textDocument": {"uri": uri},
                "position": call,
                "context": {"includeDeclaration": true},
            }),
        ),
    );
    assert_eq!(references.as_array().unwrap().len(), 2, "declaration and call");
    let completion = check("completion", timed(&mut client, "textDocument/completion", at(&call)));
    // The cursor follows `f`: every `f{index}` function matches.
    assert!(completion.as_array().is_some_and(|items| items.len() >= 333), "{completion}");
    let symbols = check(
        "documentSymbol",
        timed(&mut client, "textDocument/documentSymbol", document.clone()),
    );
    assert_eq!(symbols.as_array().unwrap().len(), 667);
    let tokens = check(
        "semanticTokens/full",
        timed(&mut client, "textDocument/semanticTokens/full", document.clone()),
    );
    assert!(tokens["data"].as_array().is_some_and(|data| !data.is_empty()));
    check(
        "inlayHint",
        timed(&mut client, "textDocument/inlayHint", whole),
    );
    let lenses = check(
        "codeLens",
        timed(&mut client, "textDocument/codeLens", document),
    );
    assert_eq!(lenses.as_array().unwrap().len(), 666);
    check(
        "documentHighlight",
        timed(&mut client, "textDocument/documentHighlight", at(&call)),
    );

    // An edit is answered from a fresh analysis just as fast.
    let edited = source.replace("f166(row.id, 166)", "f166(row.id, 167)");
    client.change(uri, 2, &edited);
    check("hover after an edit", timed(&mut client, "textDocument/hover", at(&call)));

    for (name, elapsed) in &timings {
        eprintln!("{name}: {} ms", elapsed.as_millis());
    }
    for (name, elapsed) in &timings {
        assert!(*elapsed < LIMIT, "{name} took {} ms", elapsed.as_millis());
    }
    client.shutdown();
}

#[test]
fn requests_sent_together_are_all_answered_while_a_slow_one_runs() {
    let source = module();
    let uri = "file:///workspace/concurrent-module.orna";
    let mut client = Client::start();
    client.open(uri, &source);
    let document = json!({"textDocument": {"uri": uri}});
    let call = position_of(&source, "f12(row.id", 1);
    // Send without waiting: the slow full-token request first.
    let tokens = client.send_request("textDocument/semanticTokens/full", document.clone());
    let hover = client.send_request(
        "textDocument/hover",
        json!({"textDocument": {"uri": uri}, "position": call}),
    );
    let symbols = client.send_request("textDocument/documentSymbol", document);
    let cancelled = client.send_request("workspace/symbol", json!({"query": "f"}));
    client.notify("$/cancelRequest", json!({"id": cancelled}));
    for id in [tokens, hover, symbols] {
        let response = client.response(id);
        assert!(response["error"].is_null(), "{response}");
    }
    // A cancelled request is still answered: with its result if it already
    // finished, otherwise with RequestCancelled.
    let response = client.response(cancelled);
    assert!(
        response["error"].is_null() || response["error"]["code"] == -32800,
        "{response}"
    );
    client.shutdown();
}
