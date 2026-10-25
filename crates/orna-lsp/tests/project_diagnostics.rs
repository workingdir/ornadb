//! U03-01: the language server publishes the diagnostics `orna check` reports
//! for an Orna project, computed from the open editor buffers.
//!
//! The expected codes and messages are the ones `orna check` prints for the
//! same programs (as `error[E2101]` detail lines, or as the error itself for
//! ORNA-S010-IMPORT and E2100); the server runs the same loader, standard
//! import check and semantic analysis.

#[path = "support/lsp_client.rs"]
mod lsp_client;

use lsp_client::{Client, Project, range_of};
use serde_json::Value;

fn codes_and_messages(diagnostics: &[Value]) -> Vec<(String, String)> {
    diagnostics
        .iter()
        .map(|diagnostic| {
            assert_eq!(diagnostic["source"], "orna", "{diagnostic}");
            assert_eq!(diagnostic["severity"], 1, "{diagnostic}");
            (
                diagnostic["code"].as_str().unwrap().to_owned(),
                diagnostic["message"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn pair(code: &str, message: &str) -> (String, String) {
    (code.to_owned(), message.to_owned())
}

/// Opens `main.orna` of a fresh project holding `source` and returns the
/// diagnostics published for it.
fn check_main(source: &str) -> Vec<Value> {
    let project = Project::new(&[("main.orna", source)]);
    let uri = project.uri("main.orna");
    let mut client = Client::start();
    client.open(&uri, source);
    let diagnostics = client.diagnostics_at(&uri, 1);
    client.shutdown();
    diagnostics
}

#[test]
fn semantic_errors_match_orna_check_codes_messages_and_declarations() {
    let source = "pub fn fine(): Int = 1;\npub fn uses(): Int = missing_name + 1;\n";
    let diagnostics = check_main(source);
    assert_eq!(
        codes_and_messages(&diagnostics),
        [pair("ORNA-S012-UNRESOLVED", "name cannot be resolved")]
    );
    assert_eq!(diagnostics[0]["range"], range_of(source, "uses"));

    let source = "pub fn wrong(): Int = \"text\";\n";
    let diagnostics = check_main(source);
    assert_eq!(
        codes_and_messages(&diagnostics),
        [pair("ORNA-S021-TYPE", "static types are incompatible")]
    );
    assert_eq!(diagnostics[0]["range"], range_of(source, "wrong"));

    let source = "pub fn add(left: Int, right: Int): Int = left + right;\n\
                  pub fn one(): Int = add(1);\n";
    let diagnostics = check_main(source);
    assert_eq!(
        codes_and_messages(&diagnostics),
        [pair(
            "ORNA-S021-TYPE",
            "named function arguments do not match its static signature"
        )]
    );
    assert_eq!(diagnostics[0]["range"], range_of(source, "one"));
}

#[test]
fn valid_project_has_no_diagnostics_and_syntax_errors_keep_their_spans() {
    let source = "pub fn add(left: Int, right: Int): Int = left + right;\n";
    assert!(check_main(source).is_empty());

    // `orna check` stops at E2100 here; the editor shows the parser error
    // that made the module unloadable, at its exact span.
    let source = "pub fn broken(): Int = ;\n";
    let diagnostics = check_main(source);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0]["code"], "ORNA-PARSE-001");
    assert_eq!(diagnostics[0]["range"], range_of(source, ";"));
}

#[test]
fn uncaptured_standard_imports_are_rejected_like_orna_check() {
    let source = "use std.math;\npub fn magnitude(value: Int): Int = math.abs(value);\n";
    let diagnostics = check_main(source);
    assert_eq!(
        codes_and_messages(&diagnostics),
        [pair(
            "ORNA-S010-IMPORT",
            "imported module is unavailable\n\n\
             Help: use a captured standard dependency or remove the import"
        )]
    );
    assert_eq!(diagnostics[0]["range"], range_of(source, "std.math"));
}

#[test]
fn unloadable_project_reports_e2100_on_the_root_module() {
    let source = "use nosuch;\npub fn main(): Int = 1;\n";
    let diagnostics = check_main(source);
    assert_eq!(
        codes_and_messages(&diagnostics),
        [pair(
            "E2100",
            "project source could not be loaded: imported source module is unavailable\n\n\
             Help: fix the project module graph and source boundaries, then run check again"
        )]
    );
}

#[test]
fn closed_modules_are_checked_from_disk_and_open_buffers_win() {
    let main = "use util;\npub fn total(): Int = util.helper() + 1;\n";
    let broken_util = "pub fn helper(): Int = \"text\";\n";
    let fixed_util = "pub fn helper(): Int = 2;\n";
    let project = Project::new(&[("main.orna", main), ("util.orna", broken_util)]);
    let main_uri = project.uri("main.orna");
    let util_uri = project.uri("util.orna");
    let mut client = Client::start();

    client.open(&main_uri, main);
    assert!(client.diagnostics_at(&main_uri, 1).is_empty());
    let closed = client.diagnostics(&util_uri);
    assert_eq!(closed["version"], Value::Null, "{closed}");
    let closed = closed["diagnostics"].as_array().unwrap();
    assert_eq!(
        codes_and_messages(closed),
        [pair("ORNA-S021-TYPE", "static types are incompatible")]
    );
    assert_eq!(closed[0]["range"], range_of(broken_util, "helper"));

    // The editor buffer, not the saved file, is what gets checked.
    client.open(&util_uri, broken_util);
    assert_eq!(client.diagnostics_at(&util_uri, 1).len(), 1);
    client.change(&util_uri, 2, fixed_util);
    assert!(client.diagnostics_at(&util_uri, 2).is_empty());

    // An unsaved edit that breaks the importer is reported in the importer.
    client.change(
        &main_uri,
        2,
        "use util;\npub fn total(): Int = util.missing();\n",
    );
    let diagnostics = client.diagnostics_at(&main_uri, 2);
    assert_eq!(
        codes_and_messages(&diagnostics),
        [pair(
            "ORNA-S012-UNRESOLVED",
            "module member cannot be resolved"
        )]
    );

    // Closing the fixed buffer falls back to the broken saved file.
    client.notify(
        "textDocument/didClose",
        serde_json::json!({"textDocument":{"uri":util_uri}}),
    );
    let reverted = client.diagnostics(&util_uri);
    assert_eq!(reverted["version"], Value::Null, "{reverted}");
    assert_eq!(reverted["diagnostics"].as_array().unwrap().len(), 1);
    client.shutdown();
}

#[test]
fn pull_diagnostics_return_the_published_project_diagnostics() {
    let source = "pub fn fine(): Int = 1;\npub fn uses(): Int = missing_name + 1;\n";
    let project = Project::new(&[("main.orna", source)]);
    let uri = project.uri("main.orna");
    let mut client = Client::start();
    client.open(&uri, source);
    let published = client.diagnostics_at(&uri, 1);
    let pulled = client.result(
        "textDocument/diagnostic",
        serde_json::json!({"textDocument":{"uri":uri}}),
    );
    assert_eq!(pulled["kind"], "full");
    assert_eq!(pulled["items"].as_array().unwrap(), &published);
    client.shutdown();
}

/// Runs `git` in `directory` with a pinned identity so commits never depend
/// on the host configuration.
fn git(directory: &std::path::Path, arguments: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(directory)
        .args(["-c", "user.name=kierandrewett", "-c", "user.email=kieran@drewett.dev"])
        .args(["-c", "commit.gpgsign=false"])
        .args(arguments)
        .output()
        .expect("git process");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 git output").trim().to_owned()
}

/// A project whose `stdlib/std` gitlink pins a committed snapshot holding
/// `standard`, with `files` committed beside it: the captured standard
/// dependency `orna check` expects.
fn captured_standard_project(standard: &[(&str, &str)], files: &[(&str, &str)]) -> Project {
    let project = Project::new(&[]);
    let root = project.root().to_path_buf();
    let module = root.join("stdlib/std");
    std::fs::create_dir_all(&module).expect("standard module directory");
    orna_repository_v1::initialize_repository(&module).expect("standard module repository");
    for (name, source) in standard {
        std::fs::write(module.join(name), source).expect("standard source");
    }
    git(&module, &["add", "-A"]);
    git(&module, &["commit", "--quiet", "-m", "standard module snapshot"]);
    let snapshot = git(&module, &["rev-parse", "HEAD"]);

    std::fs::write(
        root.join(".gitmodules"),
        "[submodule \"std\"]\n\tpath = stdlib/std\n\turl = https://github.com/workingdir/ornadb-std.git\n",
    )
    .expect("pinned module declaration");
    for (name, source) in files {
        project.write(name, source);
    }
    let mut add = vec![".gitmodules", ".orna"];
    add.extend(files.iter().map(|(name, _)| *name));
    let mut stage = vec!["add"];
    stage.extend(add);
    git(&root, &stage);
    git(
        &root,
        &["update-index", "--add", "--cacheinfo", &format!("160000,{snapshot},stdlib/std")],
    );
    git(&root, &["commit", "--quiet", "-m", "capture std snapshot"]);
    project
}

const CAPTURED_STD: [(&str, &str); 2] = [
    ("main.orna", "use math;\npub fn version(): Str = \"1.0.0\";\n"),
    ("math.orna", "pub fn increment(value: Int): Int = value + 1;\n"),
];

/// Opens `main.orna` of a captured standard project and returns the published
/// diagnostics.
fn check_captured_main(project: &Project, source: &str) -> Vec<Value> {
    let uri = project.uri("main.orna");
    let mut client = Client::start();
    client.open(&uri, source);
    let diagnostics = client.diagnostics_at(&uri, 1);
    client.shutdown();
    diagnostics
}

#[test]
fn captured_standard_project_without_standard_imports_has_no_diagnostics() {
    // Profile captured, no standard import: the production catalogue must
    // resolve exactly as `orna check` does, with no diagnostics.
    let source = "pub fn answer(): Int = 41;\n";
    let project = captured_standard_project(&CAPTURED_STD, &[("main.orna", source)]);
    assert!(check_captured_main(&project, source).is_empty());
}

#[test]
fn captured_standard_import_is_checked_against_the_production_catalogue() {
    // The pinned `std.math` declarations must resolve through the production
    // catalogue, which the core-only catalogue alone cannot provide.
    let source = "use std.math.{increment};\npub fn answer(): Int = increment(41);\n";
    let project = captured_standard_project(&CAPTURED_STD, &[("main.orna", source)]);
    assert!(check_captured_main(&project, source).is_empty());
}
