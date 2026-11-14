//! G16 / ornadb-csenl.16: the language server's semantic diagnostics follow the
//! `ORNA_LSP_HIR` and `ORNA_LSP_SHADOW` switches (`docs/design/hir-cutover.md`).
//!
//! The default checker and shadow mode publish the same diagnostics (code and
//! range); shadow mode only writes the differences to stderr, which the test
//! client discards, so the shadow report itself is covered by the unit tests of
//! `check_backend`. HIR does not yet match the checker on these two documents:
//! the expected HIR output is pinned here as the parity gap G8 of
//! `hir-cutover.md`, and must change when that gap closes.

#[path = "support/lsp_client.rs"]
mod lsp_client;

use std::path::Path;

use lsp_client::{Client, Project, range_of};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug)]
enum Backend {
    Default,
    Hir,
    Shadow,
}

impl Backend {
    /// The server inherits this process's environment, so both switches are
    /// always set explicitly: a parent `ORNA_LSP_HIR` or `ORNA_LSP_SHADOW` must
    /// not change the backend under test.
    fn start(self) -> Client {
        let off = Path::new("0");
        let on = Path::new("1");
        let env: [(&str, &Path); 2] = match self {
            Self::Default => [("ORNA_LSP_HIR", off), ("ORNA_LSP_SHADOW", off)],
            Self::Hir => [("ORNA_LSP_HIR", on), ("ORNA_LSP_SHADOW", off)],
            Self::Shadow => [("ORNA_LSP_HIR", off), ("ORNA_LSP_SHADOW", on)],
        };
        Client::start_with_env(&env)
    }
}

/// The `(code, range)` pairs published for `main.orna` of a fresh project that
/// holds `source`.
fn published(backend: Backend, source: &str) -> Vec<Value> {
    let project = Project::new(&[("main.orna", source)]);
    let uri = project.uri("main.orna");
    let mut client = backend.start();
    client.open(&uri, source);
    let diagnostics = client.diagnostics_at(&uri, 1);
    client.shutdown();
    diagnostics
        .iter()
        .map(|diagnostic| {
            assert_eq!(diagnostic["source"], "orna", "{backend:?}: {diagnostic}");
            json!({"code": diagnostic["code"], "range": diagnostic["range"]})
        })
        .collect()
}

fn assert_published(backend: Backend, source: &str, expected: &[Value]) {
    assert_eq!(
        published(backend, source),
        expected,
        "backend {backend:?} for {source:?}"
    );
}

#[test]
fn valid_document_publishes_no_semantic_diagnostics_under_every_backend() {
    let source = "pub fn fine(): Int = 1;\n";
    for backend in [Backend::Default, Backend::Hir, Backend::Shadow] {
        assert_published(backend, source, &[]);
    }
}

#[test]
fn unresolved_name_is_published_by_the_checker_and_by_hir_at_the_same_range() {
    let source = "pub fn fine(): Int = 1;\npub fn uses(): Int = missing_name + 1;\n";
    let expected = [json!({
        "code": "ORNA-S012-UNRESOLVED",
        "range": range_of(source, "uses"),
    })];
    assert_published(Backend::Default, source, &expected);
    assert_published(Backend::Shadow, source, &expected);
    assert_published(Backend::Hir, source, &expected);
}

#[test]
fn type_mismatch_is_published_by_the_checker_and_shadow_and_by_hir_as_an_inference_error() {
    let source = "pub fn wrong(): Int = \"text\";\n";
    let checker = [json!({
        "code": "ORNA-S021-TYPE",
        "range": range_of(source, "wrong"),
    })];
    // Gap G8: HIR keeps its own code (ORNA-INFER-011) but takes the checker's
    // item anchor, so only the code differs from the checker's ORNA-S021-TYPE.
    let hir = [json!({
        "code": "ORNA-INFER-011",
        "range": range_of(source, "wrong"),
    })];
    assert_published(Backend::Default, source, &checker);
    assert_published(Backend::Shadow, source, &checker);
    assert_published(Backend::Hir, source, &hir);
}
