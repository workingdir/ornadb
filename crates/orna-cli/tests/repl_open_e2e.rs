//! `orna repl` `:open NAME` starts the editor that `$EDITOR` names, at the line
//! the declaration starts on. The session keeps each declaration's file and
//! byte span, so the line is read from the file as it is now, and a file
//! changed since the session loaded it is refused. `$VISUAL` is never
//! consulted. Each run feeds stdin from a writer thread so the child never
//! blocks on a full pipe.

use std::{
    ffi::OsStr,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{Mutex, PoisonError},
};

use orna_cli_testkit::format3;

/// `double` starts on line 6, after the `Reading` table, a blank line and its
/// doc comment.
const REPL_SOURCE: &str = r"pub table Reading(id: Int) {
    temperature: Int,
}

/// Doubles an integer.
pub fn double(value: Int): Int = value * 2;
";

/// Held while a fake editor is written and while a REPL is spawned. A child
/// forked while a script is open for writing keeps that descriptor until it
/// execs, and running the script then fails with `ETXTBSY`.
static FORKS: Mutex<()> = Mutex::new(());

/// Writes an executable `/bin/sh` script at `scratch/name` whose body is `body`.
fn fake_editor(scratch: &Path, name: &str, body: &str) -> PathBuf {
    let path = scratch.join(name);
    {
        let _writing = FORKS.lock().unwrap_or_else(PoisonError::into_inner);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write fake editor");
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("make fake editor executable");
    path
}

/// The script body that records its arguments, one per line, at `marker`.
fn recording(marker: &Path) -> String {
    format!("printf '%s\\n' \"$@\" > '{}'", marker.display())
}

/// Runs `orna repl` on `directory` with `input`, with `EDITOR` and `VISUAL`
/// set to `editor` and `visual`, or unset when `None`.
fn repl(directory: &Path, input: &str, editor: Option<&OsStr>, visual: Option<&OsStr>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match editor {
        Some(value) => command.env("EDITOR", value),
        None => command.env_remove("EDITOR"),
    };
    match visual {
        Some(value) => command.env("VISUAL", value),
        None => command.env_remove("VISUAL"),
    };
    let mut child = {
        let _forking = FORKS.lock().unwrap_or_else(PoisonError::into_inner);
        command.spawn().expect("the REPL starts")
    };
    let stdin = child.stdin.take().expect("REPL stdin");
    let payload = format!("{input}\n:quit\n");
    let writer = std::thread::spawn(move || {
        let mut stdin = stdin;
        stdin.write_all(payload.as_bytes())
    });
    let output = child.wait_with_output().expect("the REPL exits");
    writer
        .join()
        .expect("REPL writer thread joins")
        .expect("write REPL input");
    output
}

fn combined(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn open_starts_the_editor_at_the_first_line_of_the_declaration() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let scratch = tempfile::tempdir().expect("scratch directory");
    let marker = scratch.path().join("editor-ran");
    let editor = fake_editor(scratch.path(), "editor", &recording(&marker));
    let output = repl(
        directory.path(),
        ":open double",
        Some(editor.as_os_str()),
        Some(editor.as_os_str()),
    );
    let text = combined(&output);
    assert!(output.status.success(), "{text}");
    let record = std::fs::read_to_string(&marker).expect("the editor ran");
    let mut arguments = record.lines();
    assert_eq!(arguments.next(), Some("+6"), "{record}");
    let opened = PathBuf::from(arguments.next().expect("the editor was given a path"));
    assert_eq!(
        std::fs::canonicalize(&opened).expect("the opened file exists"),
        std::fs::canonicalize(directory.path().join("main.orna")).expect("main.orna exists"),
        "{record}"
    );
    assert_eq!(arguments.next(), None, "{record}");
}

#[test]
fn open_refuses_a_name_the_session_does_not_declare() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let scratch = tempfile::tempdir().expect("scratch directory");
    let marker = scratch.path().join("editor-ran");
    let editor = fake_editor(scratch.path(), "editor", &recording(&marker));
    let output = repl(
        directory.path(),
        ":open nosuch",
        Some(editor.as_os_str()),
        Some(editor.as_os_str()),
    );
    let text = combined(&output);
    assert!(
        text.contains("no declaration named `nosuch` in this session"),
        "{text}"
    );
    assert!(text.contains("error[ORNA-REPL-COMMAND]"), "{text}");
    assert!(!marker.exists(), "a refused `:open` started the editor");
}

#[test]
fn open_without_a_name_prints_usage() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let scratch = tempfile::tempdir().expect("scratch directory");
    let marker = scratch.path().join("editor-ran");
    let editor = fake_editor(scratch.path(), "editor", &recording(&marker));
    let output = repl(
        directory.path(),
        ":open",
        Some(editor.as_os_str()),
        Some(editor.as_os_str()),
    );
    let text = combined(&output);
    assert!(text.contains("usage: :open NAME"), "{text}");
    assert!(text.contains("error[ORNA-REPL-COMMAND]"), "{text}");
    assert!(!marker.exists(), "`:open` started the editor");
}

#[test]
fn open_refuses_when_editor_is_unset_though_visual_is_set() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let scratch = tempfile::tempdir().expect("scratch directory");
    let marker = scratch.path().join("editor-ran");
    let editor = fake_editor(scratch.path(), "editor", &recording(&marker));
    let output = repl(
        directory.path(),
        ":open double",
        None,
        Some(editor.as_os_str()),
    );
    let text = combined(&output);
    assert!(text.contains("EDITOR is not set"), "{text}");
    assert!(
        !marker.exists(),
        "`:open` used `$VISUAL` in place of `$EDITOR`"
    );
}

#[test]
fn open_refuses_an_empty_editor() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(directory.path(), ":open double", Some(OsStr::new("")), None);
    let text = combined(&output);
    assert!(text.contains("EDITOR is not set"), "{text}");
}

#[test]
fn open_reports_an_editor_that_cannot_start() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let output = repl(
        directory.path(),
        ":open double",
        Some(OsStr::new("/nonexistent/orna-test-editor")),
        None,
    );
    let text = combined(&output);
    assert!(text.contains("the editor could not be started"), "{text}");
}

#[test]
fn open_reports_an_editor_that_fails() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let scratch = tempfile::tempdir().expect("scratch directory");
    let marker = scratch.path().join("editor-ran");
    let body = format!("{}; exit 3", recording(&marker));
    let editor = fake_editor(scratch.path(), "editor", &body);
    let output = repl(
        directory.path(),
        ":open double",
        Some(editor.as_os_str()),
        None,
    );
    let text = combined(&output);
    assert!(marker.exists(), "the failing editor was not started");
    assert!(
        text.contains("the editor did not finish successfully"),
        "{text}"
    );
}

#[test]
fn open_refuses_a_declaration_whose_file_changed_since_the_session_loaded_it() {
    let (directory, _repository, _) = format3::format3_repository_for_table(REPL_SOURCE, "Reading");
    let scratch = tempfile::tempdir().expect("scratch directory");
    // Prepends a line, so the byte span the session kept no longer holds the
    // declaration. The first `:open` edits the file; the second is refused.
    let editor = fake_editor(
        scratch.path(),
        "editor",
        r#"{ printf '// edited\n'; cat "$2"; } > "$2.new" && mv "$2.new" "$2""#,
    );
    let output = repl(
        directory.path(),
        ":open double\n:open double",
        Some(editor.as_os_str()),
        Some(editor.as_os_str()),
    );
    let text = combined(&output);
    assert!(
        text.contains("changed after this session loaded it; restart the REPL to open `double`"),
        "{text}"
    );
}

/// Initialises `directory` as an Orna repository, as the other CLI tests do.
fn initialize(directory: &Path) {
    let output = Command::new(env!("CARGO_BIN_EXE_orna"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["init", directory.to_str().expect("UTF-8 path")])
        .output()
        .expect("CLI initialization");
    assert!(output.status.success(), "init stderr: {:?}", output.stderr);
}

/// Runs `git` in `directory` and returns its trimmed stdout.
fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("Git process");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 Git output")
        .trim()
        .to_owned()
}

/// Gives `directory` the test identity, which CI does not provide.
fn identify(directory: &Path) {
    git(directory, &["config", "user.email", "kieran@drewett.dev"]);
    git(directory, &["config", "user.name", "kierandrewett"]);
    git(directory, &["config", "commit.gpgsign", "false"]);
}

/// Builds the pinned standard library as `project_runtime.rs` does: `stdlib/std`
/// is its own repository, committed once, and the project records that commit as
/// a gitlink beside the checkout. `std.math.increment` is therefore read from the
/// snapshot, and the session holds no working-tree file for it.
fn standard_project() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("project");
    let standard = directory.path().join("stdlib/std");
    std::fs::create_dir_all(&standard).expect("standard module directory");
    initialize(&standard);
    std::fs::write(
        standard.join("main.orna"),
        include_str!("fixtures/captured-standard-main.orna"),
    )
    .expect("standard root module");
    std::fs::write(
        standard.join("math.orna"),
        include_str!("fixtures/captured-standard-math.orna"),
    )
    .expect("standard math module");
    identify(&standard);
    git(&standard, &["add", "-A"]);
    git(
        &standard,
        &["commit", "--quiet", "-m", "standard module snapshot"],
    );
    let pin = git(&standard, &["rev-parse", "HEAD"]);

    std::fs::write(
        directory.path().join("main.orna"),
        include_str!("fixtures/captured-standard-parent.orna"),
    )
    .expect("project root module");
    std::fs::write(
        directory.path().join(".gitmodules"),
        "[submodule \"std\"]\n\tpath = stdlib/std\n\turl = https://github.com/workingdir/ornadb-std.git\n",
    )
    .expect("pinned module declaration");
    initialize(directory.path());
    identify(directory.path());
    git(
        directory.path(),
        &["add", ".gitmodules", ".orna", "main.orna"],
    );
    let gitlink = format!("160000,{pin},stdlib/std");
    git(
        directory.path(),
        &["update-index", "--add", "--cacheinfo", &gitlink],
    );
    git(
        directory.path(),
        &["commit", "--quiet", "-m", "capture std snapshot"],
    );
    directory
}

#[test]
fn open_refuses_a_standard_declaration_though_its_checkout_is_on_disk() {
    let directory = standard_project();
    assert!(
        directory.path().join("stdlib/std/math.orna").is_file(),
        "the pinned checkout is on disk beside its gitlink"
    );
    let scratch = tempfile::tempdir().expect("scratch directory");
    let marker = scratch.path().join("editor-ran");
    let editor = fake_editor(scratch.path(), "editor", &recording(&marker));
    let output = repl(
        directory.path(),
        ":open std.math.increment",
        Some(editor.as_os_str()),
        Some(editor.as_os_str()),
    );
    let text = combined(&output);
    assert!(text.contains("error[sys.source.unavailable]"), "{text}");
    assert!(
        text.contains("`std.math.increment` was not read from a file of the working tree"),
        "{text}"
    );
    assert!(!marker.exists(), "a refused `:open` started the editor");
}
