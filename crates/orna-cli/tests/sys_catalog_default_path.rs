//! The default path serves the linked `sys.catalog.*` members (`tables`,
//! `columns`) from the admitted table schemas of a project, through both
//! `orna run` and `orna repl`. A catalogue member with no linked provider is
//! refused at admission, not evaluated.

use std::{
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
};

use tempfile::TempDir;

const CATALOGUE_SOURCE: &str = r#"pub table Item(id: Str) {
    name: Str,
}

pub fn name_column(): Str {
    sys.catalog.columns | filter(c => c.name == "name") | map(c => c.name) | first() ?? "none"
}
"#;

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path")])
        .args(arguments)
        .output()
        .expect("CLI process")
}

fn git(directory: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=orna-test",
            "-c",
            "user.email=orna-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .status()
        .expect("git process");
    assert!(status.success(), "git {arguments:?}: {status:?}");
}

/// A project with an admitted table, published with `orna commit` so the
/// REPL and table reads have committed identities to serve.
fn committed_project(source: &str) -> TempDir {
    let directory = tempfile::tempdir().expect("project directory");
    let init = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.path().to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(init.status.success(), "init: {init:?}");
    std::fs::write(directory.path().join("main.orna"), source).expect("project source");
    git(directory.path(), &["init", "-q"]);
    git(directory.path(), &["add", "-A"]);
    let committed = orna(directory.path(), &["commit", "-m", "publish catalogue"]);
    assert!(committed.status.success(), "commit: {committed:?}");
    directory
}

fn repl(directory: &Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the REPL starts");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write REPL input");
    child.wait_with_output().expect("the REPL exits")
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn repl_reads_the_declared_column_catalogue_of_a_committed_project() {
    let project = committed_project(CATALOGUE_SOURCE);
    let output = repl(
        project.path(),
        r#"sys.catalog.columns | filter(c => c.name == "name") | map(c => c.table)"#,
    );
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the read: {text}"
    );
    assert!(
        text.contains("\"Item\""),
        "the column's owning table is not served: {text}"
    );
}

#[test]
fn repl_nested_table_columns_carry_the_declared_column_fields() {
    let project = committed_project(CATALOGUE_SOURCE);
    let output = repl(project.path(), "sys.catalog.tables | map(t => t.columns)");
    let text = combined(&output);
    assert!(
        !text.contains("error["),
        "the REPL refused the read: {text}"
    );
    assert!(
        text.contains("type"),
        "nested columns lack the declared `type` field: {text}"
    );
    assert!(
        !text.contains("\"ty\""),
        "nested columns still serve the undeclared `ty` field: {text}"
    );
}

#[test]
fn repl_refuses_an_unlinked_catalogue_member_at_admission() {
    // `sys.catalog.functions` has no linked provider (ORNA-SYS-008). A relation
    // pipeline over it is refused by intrinsic inference (`ORNA-S022-UNSUPPORTED`)
    // before evaluation, so no rows are served. The spec's `sys.version.incompatible`
    // code is produced only on the call-form path; this test pins the refusal, not the code.
    let project = committed_project(CATALOGUE_SOURCE);
    let output = repl(project.path(), "sys.catalog.functions | map(f => f.name)");
    let text = combined(&output);
    assert!(
        text.contains("error["),
        "an unlinked catalogue member must be refused at admission: {text}"
    );
}

#[test]
fn orna_run_reads_the_declared_column_catalogue() {
    let project = committed_project(CATALOGUE_SOURCE);
    let output = orna(project.path(), &["run", "main.name_column"]);
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "\"name\" : Str\n",
        "{text}"
    );
}
