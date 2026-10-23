use std::{
    io::{Read as _, Write},
    process::{Child, ChildStdout, Command, Output, Stdio},
};

use orna_evaluator_v1::{AdmittedReplSession, Limits};
use orna_runtime_v1::{RuntimeState, RuntimeTableIdentity, TableObjectId};
use orna_semantic_v1::{Catalogue, ModuleInput};
use orna_value_v1::{Blob, ContextValue, Raw, ValueFormat};

const MUSIC_SOURCE: &str = r#"pub table media(id: Str) {
    title: Str,
    content: Blob,
}

pub fn add(id: Str, title: Str, content: Blob) =
    media.insert({ id: id, title: title, content: content });
"#;

fn run_repl(directory: &std::path::Path, input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Orna REPL process");
    child
        .stdin
        .take()
        .expect("REPL stdin")
        .write_all(format!("{input}\n:quit\n").as_bytes())
        .expect("write trusted REPL submission");
    child.wait_with_output().expect("wait for Orna REPL")
}

fn start_repl(directory: &std::path::Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_orna"))
        .args(["--db", directory.to_str().expect("UTF-8 path"), "repl"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Orna REPL process")
}

fn read_prompt(stdout: &mut ChildStdout) -> Vec<u8> {
    let mut output = Vec::new();
    while !output.ends_with(b"> ") {
        let mut byte = [0];
        stdout.read_exact(&mut byte).expect("read REPL prompt");
        output.push(byte[0]);
    }
    output
}

fn scratch_refs(directory: &std::path::Path) -> Vec<String> {
    let output = Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/orna/pins"])
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("list repository pin refs");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("Git ref names are UTF-8")
        .lines()
        .filter(|reference| reference.contains("/scratch/"))
        .map(str::to_owned)
        .collect()
}

fn protected_ref_owner_id(directory: &std::path::Path) -> [u8; 16] {
    let output = Command::new("git")
        .args(["for-each-ref", "--format=%(refname)", "refs/orna/pins"])
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("list repository protected-content refs");
    assert!(output.status.success());
    let references = String::from_utf8(output.stdout).expect("Git ref names are UTF-8");
    let references = references.lines().collect::<Vec<_>>();
    assert_eq!(
        references.len(),
        1,
        "captured row has exactly one protected-content pin ref"
    );
    let owner_hex = references[0]
        .strip_prefix("refs/orna/pins/")
        .and_then(|reference| reference.split('/').next())
        .expect("captured row retains an owner-scoped protected-content ref");
    assert_eq!(owner_hex.len(), 32, "owner ID is a 16-byte hexadecimal value");
    let mut owner_id = [0; 16];
    for (index, pair) in owner_hex.as_bytes().chunks_exact(2).enumerate() {
        owner_id[index] = u8::from_str_radix(
            std::str::from_utf8(pair).expect("owner reference is ASCII"),
            16,
        )
        .expect("owner reference is hexadecimal");
    }
    owner_id
}

fn capture_input(root: &std::path::Path) -> String {
    format!(
        "let raw = sys.blob.capture_file({root:?}, \"recording.mp3\", 64);",
        root = root.to_string_lossy().as_ref(),
    )
}

fn git_head(directory: &std::path::Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("read Git HEAD");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("HEAD text")
        .trim()
        .to_owned()
}

async fn committed_media_row(
    state: &RuntimeState,
    relation_id: [u8; 16],
    key: &str,
) -> Option<Vec<u8>> {
    let logical_key = key.as_bytes();
    state
        .committed_table_rows(&format!("oid:{}", relation_id.iter().map(|b| format!("{b:02x}")).collect::<String>()))
        .await
        .expect("read CWD media rows")
        .into_iter()
        .find_map(|(row_key, row)| (row_key.as_slice() == logical_key).then_some(row))
}

#[tokio::test]
async fn repl_capture_annotate_and_add_commit_one_durable_blob_row_without_git_commit() {
    let (directory, repository, relation_id) = format3_repository(MUSIC_SOURCE);
    let root = directory.path();
    let payload = b"synthetic audio captured by the trusted REPL";
    std::fs::write(root.join("recording.mp3"), payload).expect("write synthetic media");
    let head_before = git_head(root);

    let input = format!(
        "let raw = sys.blob.capture_file({root:?}, \"recording.mp3\", 64);\nlet audio = sys.blob.annotate(raw, \"audio/mpeg\");\nmain.add(\"london\", \"Live in London\", audio).id",
        root = root.to_string_lossy().as_ref(),
    );
    let output = run_repl(root, &input);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}\n{:?}", output.stderr);
    assert!(
        !stdout.contains("ORNA-REPL-EFFECT"),
        "trusted REPL rejected the captured insert: {stdout}"
    );
    assert!(
        !stdout.contains("ORNA-EVAL-VALUE"),
        "trusted REPL could not evaluate the captured insert: {stdout}"
    );
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert_eq!(git_head(root), head_before, "REPL must not publish a Git commit");
    let key_json = "\"london\"";
    let typed_key = stdout.find(key_json).and_then(|start| {
        let end = start + key_json.len();
        serde_json::from_str::<String>(&stdout[start..end]).ok()
    });
    assert_eq!(
        typed_key.as_deref(),
        Some("london"),
        "logical row key should be a Str in the REPL result: {stdout}"
    );

    let runtime_owner_id = protected_ref_owner_id(root);
    let (identity, initial_digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("reopen the durable runtime after the REPL process exits");
    let encoded_row = committed_media_row(&state, relation_id, "london")
        .await
        .unwrap_or_else(|| panic!("committed media row exists; REPL output: {stdout}"));
    let format = repository
        .open_format_context()
        .expect("open committed format-3 context")
        .with_runtime_owner_id(runtime_owner_id);
    let value_context = format
        .open_value_context(relation_id)
        .expect("issue native-graph-backed value context");
    let row = ContextValue::decode_in_context(&encoded_row, ValueFormat::Rov3, &value_context)
        .expect("decode the stored row in its repository context");
    let Raw::Array(fields) = row.raw() else {
        panic!("stored Music row is not an array");
    };
    assert_eq!(fields.len(), 2, "the primary key is stored in the index, not the row array");
    let mut title_field_count = 0;
    let mut blob_field_count = 0;
    let mut blob_field = None;
    for field in fields {
        match field {
            Raw::Text(title) => {
                title_field_count += 1;
                assert_eq!(
                    title.as_str(),
                    "Live in London",
                    "stored Music title matches the inserted row"
                );
            }
            _ => {
                let value = ContextValue::new_in_context(&value_context, ValueFormat::Rov3, field.clone())
                    .expect("construct stored Music row content field");
                value.blob().expect("stored Music content field is a Blob");
                blob_field_count += 1;
                blob_field = Some(field.clone());
            }
        }
    }
    assert_eq!(
        title_field_count,
        1,
        "stored Music row has exactly one Text title field"
    );
    assert_eq!(
        blob_field_count,
        1,
        "stored Music row has exactly one Blob content field"
    );
    let encoded_blob = ContextValue::new_in_context(
        &value_context,
        ValueFormat::Rov3,
        blob_field.expect("stored Music row has a Blob content field"),
    )
    .expect("construct content field value")
    .encode()
    .expect("encode content field value");
    let blob_value = ContextValue::decode_in_context(
        &encoded_blob,
        ValueFormat::Rov3,
        &value_context,
    )
    .expect("decode the stored Blob reference");
    let blob = blob_value.blob().expect("content field is a Blob");
    let descriptor_oid = blob
        .descriptor_oid()
        .expect("stored content field retains its native descriptor")
        .clone();
    let reference = value_context
        .reference(blob.content_identity(), descriptor_oid)
        .expect("bind the descriptor to the owner-scoped repository resolver");
    let blob = Blob::from_reference_with_annotation(
        reference,
        blob.media_type(),
        blob.suffix(),
    )
    .expect("bind stored Blob metadata to the repository resolver");
    assert_eq!(blob.media_type(), "audio/mpeg");
    assert_eq!(blob.read_to_end().expect("read protected captured bytes"), payload);
}

#[tokio::test]
async fn repl_capture_add_accepts_a_key_beyond_the_legacy_path_component_limit() {
    let (directory, repository, relation_id) = format3_repository(MUSIC_SOURCE);
    let root = directory.path();
    let key = "x".repeat(201);
    std::fs::write(root.join("recording.mp3"), b"long key capture")
        .expect("write synthetic media");
    let head_before = git_head(root);
    let input = format!(
        "let raw = sys.blob.capture_file({root:?}, \"recording.mp3\", 64);\nlet audio = sys.blob.annotate(raw, \"audio/mpeg\");\nmain.add({key:?}, \"Long-key recording\", audio);",
        root = root.to_string_lossy().as_ref(),
    );

    let output = run_repl(root, &input);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}\n{:?}", output.stderr);
    assert!(
        !stdout.contains("ORNA-REPL-EFFECT"),
        "trusted REPL rejected long key: {stdout}"
    );
    assert!(
        !stdout.contains("ORNA-EVAL-VALUE"),
        "trusted REPL could not evaluate the captured long-key row: {stdout}"
    );
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    assert_eq!(git_head(root), head_before, "REPL must not publish a Git commit");

    let (identity, initial_digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("reopen runtime after long-key insert");
    assert!(
        committed_media_row(&state, relation_id, &key)
            .await
            .is_some(),
        "the typed key remains durable without a legacy loose-row filename; REPL output: {stdout}",
    );
}
#[test]
fn repl_capture_candidate_is_released_when_the_session_exits() {
    let (directory, _repository, _) = format3_repository(MUSIC_SOURCE);
    let root = directory.path();
    std::fs::write(root.join("recording.mp3"), b"session-drop capture")
        .expect("write synthetic media");

    let mut child = start_repl(root);
    let mut stdin = child.stdin.take().expect("REPL stdin");
    let mut stdout = child.stdout.take().expect("REPL stdout");
    let _initial_prompt = read_prompt(&mut stdout);
    writeln!(stdin, "{}", capture_input(root)).expect("capture file in REPL");
    stdin.flush().expect("flush capture input");
    let captured = read_prompt(&mut stdout);
    assert!(
        !String::from_utf8_lossy(&captured).contains("ORNA-"),
        "capture submission failed: {}",
        String::from_utf8_lossy(&captured)
    );
    assert_eq!(scratch_refs(root).len(), 1, "capture holds one scratch ref");

    stdin.write_all(b":quit\n").expect("quit REPL");
    stdin.flush().expect("flush quit command");
    drop(stdin);
    assert_eq!(child.wait().expect("wait for REPL exit").code(), Some(0));
    let mut trailing = Vec::new();
    stdout.read_to_end(&mut trailing).expect("read REPL output");
    assert!(scratch_refs(root).is_empty(), "session drop releases unconsumed capture");
}

#[test]
fn repl_snapshot_replacement_releases_unconsumed_capture() {
    let (directory, _repository, _) = format3_repository(MUSIC_SOURCE);
    let root = directory.path();
    std::fs::write(root.join("recording.mp3"), b"snapshot replacement capture")
        .expect("write synthetic media");

    let mut child = start_repl(root);
    let mut stdin = child.stdin.take().expect("REPL stdin");
    let mut stdout = child.stdout.take().expect("REPL stdout");
    let _initial_prompt = read_prompt(&mut stdout);
    writeln!(stdin, "{}", capture_input(root)).expect("capture file in REPL");
    stdin.flush().expect("flush capture input");
    let captured = read_prompt(&mut stdout);
    assert!(!String::from_utf8_lossy(&captured).contains("ORNA-"));
    assert_eq!(scratch_refs(root).len(), 1, "capture holds one scratch ref");

    stdin.write_all(b":at HEAD\n").expect("select committed snapshot");
    stdin.flush().expect("flush snapshot command");
    let replaced = read_prompt(&mut stdout);
    assert!(
        !String::from_utf8_lossy(&replaced).contains("ORNA-REPL-AT"),
        "snapshot replacement failed: {}",
        String::from_utf8_lossy(&replaced)
    );
    assert!(
        scratch_refs(root).is_empty(),
        "successful snapshot replacement releases old session captures"
    );

    stdin.write_all(b":quit\n").expect("quit REPL");
    stdin.flush().expect("flush quit command");
    drop(stdin);
    assert_eq!(child.wait().expect("wait for REPL exit").code(), Some(0));
}

#[tokio::test]
async fn repl_capture_candidate_is_reclaimed_after_crash_and_runtime_reopen() {
    let (directory, repository, _) = format3_repository(MUSIC_SOURCE);
    let root = directory.path();
    std::fs::write(root.join("recording.mp3"), b"crash-recovery capture")
        .expect("write synthetic media");

    let mut child = start_repl(root);
    let mut stdin = child.stdin.take().expect("REPL stdin");
    let mut stdout = child.stdout.take().expect("REPL stdout");
    let _initial_prompt = read_prompt(&mut stdout);
    writeln!(stdin, "{}", capture_input(root)).expect("capture file in REPL");
    stdin.flush().expect("flush capture input");
    let captured = read_prompt(&mut stdout);
    assert!(!String::from_utf8_lossy(&captured).contains("ORNA-"));
    assert_eq!(scratch_refs(root).len(), 1, "capture holds one scratch ref");

    child.kill().expect("terminate the live REPL as a crashed owner");
    assert!(!child.wait().expect("reap crashed REPL").success());
    drop(stdin);
    drop(stdout);

    let (identity, initial_digest) = cli_runtime_identity(&repository);
    let state = RuntimeState::open(&repository, identity, initial_digest)
        .await
        .expect("reopen runtime to recover the crashed owner");
    assert!(
        scratch_refs(root).is_empty(),
        "runtime reopen reclaims the crashed session's unaccepted scratch ref"
    );
    drop(state);
}

#[test]
fn repl_user_main_module_wins_over_the_root_main_alias() {
    let modules = [
        ModuleInput::new("main.orna", "pub fn add(value: Int) = value + 10;"),
        ModuleInput::new("main/main.orna", "pub fn add(value: Int) = value + 1;"),
    ];
    let mut session = AdmittedReplSession::from_catalogue(
        &modules,
        Catalogue::authoritative_core(),
        std::iter::empty::<(String, String)>(),
        Limits::default(),
    )
    .expect("root and user main modules are admitted");

    assert!(session.submit("main.add(1)").is_err());
    session.submit("use main;").expect("user main module is importable");
    let value = session
        .submit("main.add(1)")
        .expect("user main function is callable")
        .expect("function returns a value");
    assert_eq!(value.raw(), &Raw::Int(2.into()));
}
#[test]
fn repl_root_main_alias_targets_root_namespace_without_user_main() {
    let modules = [ModuleInput::new(
        "main.orna",
        "pub fn add(value: Int) = value + 1;",
    )];
    let mut session = AdmittedReplSession::from_catalogue(
        &modules,
        Catalogue::authoritative_core(),
        std::iter::empty::<(String, String)>(),
        Limits::default(),
    )
    .expect("root module is admitted");

    let value = session
        .submit("main.add(1)")
        .expect("root main alias resolves the public root function")
        .expect("function returns a value");
    assert_eq!(value.raw(), &Raw::Int(2.into()));
}
#[path = "support/format3.rs"]
mod format3;
use format3::*;
