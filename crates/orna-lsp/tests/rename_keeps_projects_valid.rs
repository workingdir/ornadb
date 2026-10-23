//! W19-04 / W19-18: a rename the server returns leaves a valid project valid.
//!
//! A fresh `orna init` project declares a dimension, units (one used as a
//! numeral suffix), an enum matched in `case` patterns, nominal types used as
//! field types and constructors, and a function called inside string
//! interpolation, across two modules. Each rename is applied to the open
//! buffers exactly as an editor applies the WorkspaceEdit, and the server's
//! project check — the analysis `orna check` runs — must still report
//! nothing, with no identifier left naming the old symbol.

#[path = "support/lsp_client.rs"]
mod lsp_client;

use std::collections::BTreeMap;

use lsp_client::{Client, Project, byte_at, position_of};
use orna_syntax_v1::TokenKind;
use serde_json::{Value, json};

const CATALOG: &str = "pub dim Length;

pub unit metre : Length base;

pub unit kilometre : Length = 1000.metre;

pub enum Status { draft, published }

pub type Badge {
    pub text: Str,
}

pub type Article {
    pub state: Status,
    pub badge: Badge,
}

pub fn label(s: Status): Str = case s {
    Status.draft: \"Draft\",
    Status.published: \"Published\",
};
";

const MAIN: &str = "use catalog.{Article, Badge, Status, label};

pub fn make(): Badge = Badge { text: \"x\" };

pub fn greet(s: Status): Str = \"state {label(s)}\";

pub fn headline(article: Article): Str = article.badge.text;

pub fn fresh(): Article = Article { state: Status.draft, badge: make() };
";

/// Applies one document's text edits, as an editor applies a WorkspaceEdit.
fn apply(source: &str, edits: &[Value]) -> String {
    let mut spans = edits
        .iter()
        .map(|edit| {
            (
                byte_at(source, &edit["range"]["start"]),
                byte_at(source, &edit["range"]["end"]),
                edit["newText"].as_str().unwrap(),
            )
        })
        .collect::<Vec<_>>();
    spans.sort_by(|left, right| right.0.cmp(&left.0));
    let mut text = source.to_owned();
    for (start, end, replacement) in spans {
        text.replace_range(start..end, replacement);
    }
    text
}

fn identifiers(source: &str) -> Vec<String> {
    orna_syntax_v1::lex(source)
        .expect("renamed source lexes")
        .into_iter()
        .filter(|token| matches!(token.kind, TokenKind::Identifier { .. }))
        .map(|token| token.text)
        .collect()
}

struct Session {
    client: Client,
    /// Text and version of each open module, by URI.
    files: BTreeMap<String, (String, i32)>,
}

impl Session {
    /// Brings every buffer to a new version and returns the diagnostics the
    /// server publishes for each.
    fn check(&mut self) -> Vec<(String, Vec<Value>)> {
        for (uri, (text, version)) in &mut self.files {
            *version += 1;
            self.client.change(uri, *version, text);
        }
        let versions = self
            .files
            .iter()
            .map(|(uri, (_, version))| (uri.clone(), *version))
            .collect::<Vec<_>>();
        versions
            .into_iter()
            .map(|(uri, version)| {
                let diagnostics = self.client.diagnostics_at(&uri, version);
                (uri, diagnostics)
            })
            .collect()
    }

    /// Renames the name `offset` bytes into the first `needle` of `uri` and
    /// applies the returned edits.
    fn rename(&mut self, uri: &str, needle: &str, offset: usize, new_name: &str) -> usize {
        let text = self.files[uri].0.clone();
        let response = self.client.request(
            "textDocument/rename",
            json!({
                "textDocument": {"uri": uri},
                "position": position_of(&text, needle, offset),
                "newName": new_name,
            }),
        );
        assert!(
            response["error"].is_null(),
            "rename of `{needle}` to `{new_name}` was refused: {response}"
        );
        let changes = response["result"]["changes"]
            .as_object()
            .unwrap_or_else(|| panic!("rename returned no changes: {response}"));
        let mut edits = 0;
        for (changed, file_edits) in changes {
            let file_edits = file_edits.as_array().unwrap();
            edits += file_edits.len();
            let (text, _) = self
                .files
                .get_mut(changed)
                .unwrap_or_else(|| panic!("edit for a module that is not open: {changed}"));
            *text = apply(text, file_edits);
        }
        edits
    }

    fn assert_valid(&mut self, after: &str) {
        for (uri, diagnostics) in self.check() {
            assert!(
                diagnostics.is_empty(),
                "after {after}, `orna check` reports for {uri}: {diagnostics:#?}\n{}",
                self.files[&uri].0
            );
        }
    }

    fn assert_gone(&self, old_name: &str) {
        for (uri, (text, _)) in &self.files {
            assert!(
                !identifiers(text).iter().any(|name| name == old_name),
                "`{old_name}` still named in {uri}:\n{text}"
            );
        }
    }
}

#[test]
fn renames_across_modules_leave_the_project_valid() {
    let project = Project::new(&[("main.orna", MAIN), ("catalog.orna", CATALOG)]);
    let catalog = project.uri("catalog.orna");
    let main = project.uri("main.orna");
    let mut client = Client::start();
    client.open(&catalog, CATALOG);
    client.open(&main, MAIN);
    let mut session = Session {
        client,
        files: BTreeMap::from([
            (catalog.clone(), (CATALOG.to_owned(), 1)),
            (main.clone(), (MAIN.to_owned(), 1)),
        ]),
    };
    session.assert_valid("opening the project");

    // (module, text before the name, offset into it, old name, new name,
    // expected edit count)
    let renames = [
        (&catalog, "dim Length", "dim ".len(), "Length", "Distance", 3),
        (&catalog, "unit metre", "unit ".len(), "metre", "meter", 2),
        (&main, "s: Status", "s: ".len(), "Status", "State", 8),
        (&catalog, "type Badge", "type ".len(), "Badge", "Tag", 5),
        (&main, "{label(s)}", 1, "label", "title", 3),
        (&catalog, "pub badge", "pub ".len(), "badge", "tag", 3),
        (&catalog, "draft,", 0, "draft", "pending", 3),
        (&main, "text: \"x\"", 0, "text", "caption", 3),
    ];
    for (uri, needle, offset, old_name, new_name, expected) in renames {
        let edits = session.rename(uri, needle, offset, new_name);
        assert_eq!(edits, expected, "edits renaming `{old_name}` to `{new_name}`");
        session.assert_gone(old_name);
        session.assert_valid(&format!("renaming `{old_name}` to `{new_name}`"));
    }
    session.client.shutdown();
}

#[test]
fn a_rename_that_would_break_the_project_is_refused_with_a_reason() {
    let project = Project::new(&[("main.orna", MAIN), ("catalog.orna", CATALOG)]);
    let catalog = project.uri("catalog.orna");
    let main = project.uri("main.orna");
    let mut client = Client::start();
    client.open(&catalog, CATALOG);
    client.open(&main, MAIN);

    // `Badge` would collide with the existing type `Article`.
    let response = client.request(
        "textDocument/rename",
        json!({
            "textDocument": {"uri": catalog},
            "position": position_of(CATALOG, "type Badge", "type ".len()),
            "newName": "Article",
        }),
    );
    assert_eq!(response["error"]["code"], -32803, "{response}");
    let message = response["error"]["message"].as_str().unwrap();
    assert!(message.starts_with("cannot rename: "), "{message}");

    // A field renames like any other declaration; the editor is told why
    // when it cannot.
    let prepared = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": {"uri": catalog},
            "position": position_of(CATALOG, "pub badge", "pub ".len()),
        }),
    );
    assert_eq!(prepared["result"]["placeholder"], "badge", "{prepared}");
    let keyword = client.request(
        "textDocument/prepareRename",
        json!({
            "textDocument": {"uri": main},
            "position": position_of(MAIN, "pub fn make", 1),
        }),
    );
    assert_eq!(keyword["error"]["code"], -32803, "{keyword}");
    client.shutdown();
}
