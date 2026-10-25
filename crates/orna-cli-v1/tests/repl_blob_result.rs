//! A REPL line whose result is a captured format-3 Blob. The REPL presents the
//! payload-free metadata map (`type`, `length`, `sha256`, `suffix`,
//! `media_type`); it never prints the payload bytes or the descriptor OID, and
//! it never reports `ORNA-EVAL-VALUE` or `ORNA-REPL-EFFECT-UNCERTAIN` for the
//! capture. Covers a bare capture as the last expression, and four sequential
//! capture-then-insert lines whose rows `orna publish` admits.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

const MEDIA_SOURCE: &str = r#"pub table media(id: Str) {
    title: Str,
    content: Blob,
}

pub fn add(id: Str, title: Str, content: Blob) =
    media.insert({ id: id, title: title, content: content });
"#;

const PAYLOAD: &[u8] = b"bare-capture-payload-bytes";

/// Keeps the host's Git configuration and repository routing out of every
/// process this test starts.
fn isolate_git(command: &mut Command) {
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null");
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CONFIG_COUNT",
    ] {
        command.env_remove(name);
    }
}

fn orna(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    command
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("the orna process starts")
}

fn orna_repl(root: &Path, input: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna"));
    isolate_git(&mut command);
    let mut child = command
        .args(["--db", root.to_str().expect("UTF-8 test path"), "repl"])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the orna REPL starts");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write REPL input");
    child.wait_with_output().expect("the orna REPL exits")
}

fn git(root: &Path, arguments: &[&str]) -> String {
    let mut command = Command::new("git");
    isolate_git(&mut command);
    let output = command
        .args(arguments)
        .current_dir(root)
        .output()
        .expect("the git process starts");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("git prints UTF-8")
}

fn transcript(output: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn assert_success(output: &Output, step: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{step} failed\n{}",
        transcript(output)
    );
}

/// `orna init` into a fresh directory, a test-local commit identity, and
/// `main.orna` replaced by `source`.
fn initialized_database(directory: &TempDir, source: &str) -> PathBuf {
    let root = directory.path().join("library");
    let init = orna(
        directory.path(),
        &["init", root.to_str().expect("UTF-8 test path")],
    );
    assert_success(&init, "orna init");
    git(&root, &["config", "user.name", "Orna Test"]);
    git(
        &root,
        &["config", "user.email", "orna-test@example.invalid"],
    );
    git(&root, &["config", "commit.gpgsign", "false"]);
    fs::write(root.join("main.orna"), source).expect("write main.orna");
    root
}

/// Stages the source with Git and commits it with `orna commit`, returning
/// the relation id `orna commit` reports for each newly admitted table.
fn commit_source(root: &Path, message: &str) -> Vec<(String, String)> {
    git(root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(root, &["commit", "-m", message]);
    assert_success(&committed, "orna commit");
    String::from_utf8_lossy(&committed.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("admitted table "))
        .map(|admitted| {
            let (table, relation) = admitted
                .split_once(" as relation ")
                .expect("an admitted table names its relation");
            (table.to_owned(), relation.to_owned())
        })
        .collect()
}

fn query_rows(root: &Path, arguments: &[&str]) -> u64 {
    let listed = orna(root, arguments);
    assert_success(&listed, "orna query");
    let report: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("orna query prints JSON");
    report["rows"].as_u64().expect("the report counts rows")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Maximal runs of hexadecimal digits at least 40 long. A digest or an OID
/// printed by the REPL appears as one of these runs.
fn long_hex_runs(text: &str) -> Vec<&str> {
    text.split(|character: char| !character.is_ascii_hexdigit())
        .filter(|run| run.len() >= 40)
        .collect()
}

/// The refusals a captured Blob result must never report.
fn assert_no_refusal(output: &Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let transcript = transcript(output);
    assert!(output.stderr.is_empty(), "{transcript}");
    for refusal in ["ORNA-EVAL-VALUE", "ORNA-REPL-EFFECT-UNCERTAIN", "error["] {
        assert!(!stdout.contains(refusal), "{refusal}\n{transcript}");
    }
}

#[test]
fn bare_capture_result_is_presented_as_a_payload_free_summary() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, MEDIA_SOURCE);
    commit_source(&root, "Declare media");
    fs::write(root.join("recording.mp3"), PAYLOAD).expect("write the captured file");

    let captured = orna_repl(
        &root,
        &format!(
            "sys.blob.capture_file({root:?}, \"recording.mp3\", 64)",
            root = root.to_string_lossy().as_ref(),
        ),
    );
    assert_success(&captured, "REPL bare capture");
    assert_no_refusal(&captured);

    let stdout = String::from_utf8_lossy(&captured.stdout);
    let transcript = transcript(&captured);
    // The summary is a metadata map, not a Blob.
    assert!(stdout.contains(": Map"), "{transcript}");
    for key in ["Blob", "length", "sha256"] {
        assert!(stdout.contains(key), "missing {key}\n{transcript}");
    }
    // Neither the payload text nor its bytes cross the REPL boundary.
    assert!(!stdout.contains("bare-capture-payload"), "{transcript}");
    assert!(!stdout.contains(&hex(PAYLOAD)), "{transcript}");
    // The only long hex run is the payload digest: no descriptor OID appears.
    let digest = hex(&Sha256::digest(PAYLOAD));
    assert_eq!(
        long_hex_runs(&stdout),
        vec![digest.as_str()],
        "the REPL prints the digest and no descriptor OID\n{transcript}"
    );
}

#[test]
fn four_sequential_capture_and_insert_lines_publish_four_rows() {
    let directory = TempDir::new().expect("create the test directory");
    let root = initialized_database(&directory, MEDIA_SOURCE);
    let admitted = commit_source(&root, "Declare media");
    let [(table, relation)] = admitted.as_slice() else {
        panic!("orna commit admits exactly the media table: {admitted:?}");
    };
    assert_eq!(table, "media");

    let mut input = String::new();
    for index in 1..=4 {
        let name = format!("clip-{index}.mp3");
        fs::write(root.join(&name), format!("sequential capture {index}"))
            .expect("write the captured file");
        input.push_str(&format!(
            "let raw_{index} = sys.blob.capture_file({root:?}, {name:?}, 64);\n\
             let audio_{index} = sys.blob.annotate(raw_{index}, \"audio/mpeg\");\n\
             main.add(\"clip-{index}\", \"Clip {index}\", audio_{index});\n",
            root = root.to_string_lossy().as_ref(),
        ));
    }
    let captured = orna_repl(&root, input.trim_end());
    assert_success(&captured, "REPL sequential captures");
    assert_no_refusal(&captured);

    let published = orna(&root, &["publish", "--message", "Add clips"]);
    assert_success(&published, "orna publish");
    assert_eq!(
        query_rows(&root, &["query", relation, "--format", "json"]),
        4
    );
    assert_eq!(
        query_rows(
            &root,
            &["query", relation, "--key", "clip-3", "--format", "json"]
        ),
        1
    );
}
