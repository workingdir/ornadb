//! The language-server request handlers answer the same under the checker
//! default and under `ORNA_LSP_HIR=1`.
//!
//! The `ORNA_LSP_HIR` switch selects the diagnostics backend
//! (`check_backend.rs`). Hover, navigation, hierarchy, semantic-token, folding,
//! inlay, lens and workspace-symbol answers must stay identical under both
//! switches; this test pins that. Pull diagnostics also differ by the G8 code
//! gap, so they are pinned in `check_backend_gate.rs` and `project_diagnostics.rs`.
//! This test pins that per handler family, so moving any handler onto HIR
//! (criterion C7(b) of `docs/design/hir-cutover.md`) must keep its answer
//! identical under both paths.

#[path = "support/lsp_client.rs"]
mod lsp_client;

use lsp_client::{Client, Project, position_at, position_of};
use serde_json::{Value, json};
use std::path::Path;

const MAIN: &str = include_str!("fixtures/workspace-hierarchy-provider-v1.orna");
const TYPES: &str = include_str!("fixtures/type-hierarchy-provider-v1.orna");
const LENS: &str = include_str!("fixtures/document-highlight-code-lens-v1.orna");

/// Runs every handler family against one server session with the HIR backend
/// switched on or off, returning `(method, result)` in request order.
fn session(hir: bool, project: &Project) -> Vec<(&'static str, Value)> {
    let off = Path::new("0");
    let on = Path::new("1");
    let flag = if hir { on } else { off };
    let mut client = Client::start_with_env(&[("ORNA_LSP_HIR", flag), ("ORNA_LSP_SHADOW", off)]);
    let main = project.uri("main.orna");
    let types = project.uri("types.orna");
    let lens = project.uri("lens.orna");
    client.open(&main, MAIN);
    client.open(&types, TYPES);
    client.open(&lens, LENS);

    let text = json!({ "uri": main });
    let at_call = json!({
        "textDocument": { "uri": main },
        "position": position_of(MAIN, "mid(seed", 0),
    });
    let at_declaration = json!({
        "textDocument": { "uri": main },
        "position": position_of(MAIN, "seed(value: Int)", 0),
    });
    let at_type = json!({
        "textDocument": { "uri": types },
        "position": position_of(TYPES, "Document", 1),
    });
    let whole_main = json!({
        "start": position_at(MAIN, 0),
        "end": position_at(MAIN, MAIN.len()),
    });

    let mut results = Vec::new();
    let mut run = |client: &mut Client, method: &'static str, params: Value| {
        let result = client.result(method, params);
        results.push((method, result.clone()));
        result
    };

    run(
        &mut client,
        "textDocument/documentSymbol",
        json!({ "textDocument": text }),
    );
    run(&mut client, "textDocument/hover", at_call.clone());
    run(&mut client, "textDocument/definition", at_call.clone());
    run(
        &mut client,
        "textDocument/references",
        json!({ "textDocument": at_call["textDocument"], "position": at_call["position"],
                "context": { "includeDeclaration": true } }),
    );
    run(
        &mut client,
        "textDocument/documentHighlight",
        at_call.clone(),
    );
    run(&mut client, "workspace/symbol", json!({ "query": "mid" }));
    run(
        &mut client,
        "textDocument/semanticTokens/full",
        json!({ "textDocument": text }),
    );
    run(
        &mut client,
        "textDocument/foldingRange",
        json!({ "textDocument": text }),
    );
    run(
        &mut client,
        "textDocument/inlayHint",
        json!({ "textDocument": text, "range": whole_main }),
    );
    // textDocument/diagnostic is not run here: HIR and the checker publish
    // different codes for the call-of-non-function error (gap G8). Its parity
    // is pinned in check_backend_gate.rs and project_diagnostics.rs.
    run(
        &mut client,
        "textDocument/codeLens",
        json!({ "textDocument": { "uri": lens } }),
    );

    let items = run(
        &mut client,
        "textDocument/prepareCallHierarchy",
        at_declaration,
    );
    let call_item = items[0].clone();
    run(
        &mut client,
        "callHierarchy/incomingCalls",
        json!({ "item": call_item }),
    );
    run(
        &mut client,
        "callHierarchy/outgoingCalls",
        json!({ "item": call_item }),
    );

    let types_items = run(&mut client, "textDocument/prepareTypeHierarchy", at_type);
    let type_item = types_items[0].clone();
    run(
        &mut client,
        "typeHierarchy/supertypes",
        json!({ "item": type_item }),
    );
    run(
        &mut client,
        "typeHierarchy/subtypes",
        json!({ "item": type_item }),
    );

    client.shutdown();
    results
}

#[test]
fn every_handler_family_answers_identically_under_the_checker_and_hir() {
    let project = Project::new(&[
        ("main.orna", MAIN),
        ("types.orna", TYPES),
        ("lens.orna", LENS),
    ]);
    let checker = session(false, &project);
    let hir = session(true, &project);
    assert_eq!(checker.len(), hir.len());
    for ((method, expected), (hir_method, actual)) in checker.iter().zip(&hir) {
        assert_eq!(method, hir_method);
        assert_eq!(actual, expected, "{method} differs under ORNA_LSP_HIR=1");
    }
}

#[test]
fn parity_is_not_vacuous_each_answering_handler_returns_content() {
    let project = Project::new(&[
        ("main.orna", MAIN),
        ("types.orna", TYPES),
        ("lens.orna", LENS),
    ]);
    let results = session(true, &project);
    // foldingRange is excluded from the non-vacuity list: this fixture answers
    // [] for it under both switches. It is still compared by
    // every_handler_family_answers_identically_under_the_checker_and_hir.
    for method in [
        "textDocument/documentSymbol",
        "textDocument/hover",
        "textDocument/definition",
        "textDocument/references",
        "textDocument/documentHighlight",
        "workspace/symbol",
        "textDocument/semanticTokens/full",
        "textDocument/prepareCallHierarchy",
        "textDocument/prepareTypeHierarchy",
    ] {
        let (_, value) = results
            .iter()
            .find(|(name, _)| *name == method)
            .unwrap_or_else(|| panic!("{method} was not requested"));
        assert!(!value.is_null(), "{method} answered null");
        assert_ne!(value, &json!([]), "{method} answered an empty list");
    }
}
