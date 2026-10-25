//! U03-10: go-to-definition on a standard-library name opens the standard
//! source the analysis used, never the user's file.
//!
//! Outside a project that is the reference standard library this server
//! ships. In a project it is the standard dependency the repository captures
//! at `stdlib/std`, the snapshot `orna check` verifies; a project that
//! captures none has no standard definitions, as `orna check` rejects the
//! import. The server writes a read-only copy of the declaring source below
//! `$XDG_CACHE_HOME/orna/std/<digest>/` and returns its `file://` URI.

#[path = "support/lsp_client.rs"]
mod lsp_client;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use lsp_client::{Client, Project, position_of, text_at};
use serde_json::{Value, json};

const MATH_USE: &str = "use std.math.{increment};\n\
    pub fn next(value: Int): Int = std.math.increment(increment(value));\n";

fn definition(client: &mut Client, uri: &str, position: Value) -> Value {
    client.result(
        "textDocument/definition",
        json!({"textDocument": {"uri": uri}, "position": position}),
    )
}

/// The cached file a definition opens, checked to be a read-only copy below
/// `cache`.
fn cached_source(cache: &Path, location: &Value) -> (PathBuf, String) {
    let uri = location["uri"].as_str().expect("definition URI");
    assert!(uri.starts_with("file://"), "{location}");
    let path = url::Url::parse(uri).unwrap().to_file_path().unwrap();
    assert!(
        path.starts_with(cache.join("orna/std")),
        "{} is not below the cache directory",
        path.display()
    );
    assert!(fs::metadata(&path).unwrap().permissions().readonly());
    let text = fs::read_to_string(&path).unwrap();
    (path, text)
}

#[test]
fn standard_definitions_outside_a_project_open_the_reference_source() {
    let cache = tempfile::tempdir().unwrap();
    let mut client = Client::start_with_env(&[("XDG_CACHE_HOME", cache.path())]);
    let uri = "file:///workspace/standard-definition.orna";
    client.open(uri, MATH_USE);

    let qualified = definition(
        &mut client,
        uri,
        position_of(MATH_USE, "std.math.increment(", "std.math.".len() + 1),
    );
    let (path, text) = cached_source(cache.path(), &qualified);
    assert!(path.ends_with("std/math.orna"), "{}", path.display());
    assert_eq!(text_at(&text, &qualified["range"]), "increment");
    let reference = orna_standard_sources::reference_standard_sources_v1()
        .into_iter()
        .find(|(logical, _)| logical == "std/math.orna")
        .expect("reference std.math")
        .1;
    assert_eq!(text, reference, "the copy is the source the server analysed");

    // The imported name opens the same declaration.
    let imported = definition(&mut client, uri, position_of(MATH_USE, "increment(value)", 1));
    assert_eq!(imported, qualified);
    client.shutdown();
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Orna Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Orna Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

const CAPTURED_COLLECTION: &str = "/// The captured answer.\npub fn answer(): Int = 42;\n";
const ANSWER_USE: &str = "use std.collection.{answer};\npub fn twice(): Int = answer() + answer();\n";

/// Captures `stdlib/std` in `project` as `orna` records a standard
/// dependency: a submodule gitlink committed in the project.
fn capture_standard(project: &Project) {
    let module = project.root().join("stdlib/std");
    fs::create_dir_all(&module).unwrap();
    orna_repository_v1::initialize_repository(&module).expect("std repository");
    fs::write(module.join("collection.orna"), CAPTURED_COLLECTION).unwrap();
    git(&module, &["add", "-A"]);
    git(&module, &["commit", "--quiet", "-m", "standard"]);
    let commit = git(&module, &["rev-parse", "HEAD"]);

    fs::write(
        project.root().join(".gitmodules"),
        "[submodule \"std\"]\n\tpath = stdlib/std\n\turl = https://example.invalid/ornadb-std.git\n",
    )
    .unwrap();
    git(project.root(), &["add", "main.orna", ".gitmodules"]);
    git(
        project.root(),
        &["update-index", "--add", "--cacheinfo", &format!("160000,{commit},stdlib/std")],
    );
    git(project.root(), &["commit", "--quiet", "-m", "capture std"]);
}

#[test]
fn project_standard_definitions_open_the_captured_snapshot() {
    let cache = tempfile::tempdir().unwrap();
    let project = Project::new(&[("main.orna", ANSWER_USE)]);
    capture_standard(&project);
    let uri = project.uri("main.orna");
    let mut client = Client::start_with_env(&[("XDG_CACHE_HOME", cache.path())]);
    client.open(&uri, ANSWER_USE);
    let diagnostics = client.diagnostics_at(&uri, 1);
    assert!(diagnostics.is_empty(), "{diagnostics:#?}");

    let location = definition(&mut client, &uri, position_of(ANSWER_USE, "answer() +", 1));
    let (path, text) = cached_source(cache.path(), &location);
    assert!(path.ends_with("std/collection.orna"), "{}", path.display());
    assert_eq!(text, CAPTURED_COLLECTION, "the captured snapshot, not the reference library");
    assert_eq!(text_at(&text, &location["range"]), "answer");
    client.shutdown();
}

#[test]
fn a_project_without_a_captured_standard_library_has_no_standard_definitions() {
    let cache = tempfile::tempdir().unwrap();
    let project = Project::new(&[("main.orna", MATH_USE)]);
    let uri = project.uri("main.orna");
    let mut client = Client::start_with_env(&[("XDG_CACHE_HOME", cache.path())]);
    client.open(&uri, MATH_USE);
    let diagnostics = client.diagnostics_at(&uri, 1);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic["code"] == "ORNA-S010-IMPORT"),
        "`orna check` rejects the uncaptured import: {diagnostics:#?}"
    );
    let location = definition(
        &mut client,
        &uri,
        position_of(MATH_USE, "std.math.increment(", "std.math.".len() + 1),
    );
    assert_eq!(location, Value::Null);
    client.shutdown();
}
