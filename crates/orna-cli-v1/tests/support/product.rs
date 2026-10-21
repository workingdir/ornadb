//! Drives the shipped `orna` binary and plain Git against a database created
//! by `orna init`, and reads the committed store back through the public
//! repository API to check the bytes those commands produced.

#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use orna_repository_v1::{KeyRange, Repository, TypedKey};
use orna_value_v1::{Raw, decode_canonical_storage_value};
use tempfile::TempDir;

/// Keeps the host's Git configuration and repository routing out of every
/// process a test starts.
pub fn isolate_git(command: &mut Command) {
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

pub fn orna(directory: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"));
    isolate_git(&mut command);
    command
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("the orna process starts")
}

/// One REPL session that evaluates each line of `input` in order.
pub fn orna_repl(root: &Path, input: &str) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orna-cli-v1"));
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

pub fn git(root: &Path, arguments: &[&str]) -> String {
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

pub fn transcript(output: &Output) -> String {
    format!(
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

pub fn assert_success(output: &Output, step: &str) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "{step} failed\n{}",
        transcript(output)
    );
}

/// `orna init` into a fresh directory, a test-local commit identity, and
/// `main.orna` replaced by `source`.
pub fn initialized_database(directory: &TempDir, source: &str) -> PathBuf {
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

/// Stages `main.orna` and the metadata with Git and runs `orna commit`,
/// returning its output, which must succeed.
pub fn commit_source(root: &Path, message: &str) -> String {
    git(root, &["add", "main.orna", ".orna/database.orna"]);
    let committed = orna(root, &["commit", "-m", message]);
    assert_success(&committed, "orna commit");
    String::from_utf8_lossy(&committed.stdout).into_owned()
}

/// The relation id `orna commit` printed for each newly admitted table.
pub fn admitted_relations(commit_output: &str) -> BTreeMap<String, String> {
    commit_output
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

/// Inserts through one REPL session and checks that every line was accepted.
pub fn repl_writes(root: &Path, input: &str) -> String {
    let output = orna_repl(root, input);
    assert_success(&output, "REPL session");
    let text = transcript(&output);
    assert!(
        !text.contains("error["),
        "the REPL accepts every line\n{text}"
    );
    text
}

pub fn publish(root: &Path, message: &str) -> String {
    let published = orna(root, &["publish", "--message", message]);
    assert_success(&published, "orna publish");
    let report = String::from_utf8_lossy(&published.stdout).into_owned();
    assert!(
        report.starts_with("published "),
        "{}",
        transcript(&published)
    );
    report
}

pub fn query_rows(root: &Path, arguments: &[&str]) -> u64 {
    let listed = orna(root, arguments);
    assert_success(&listed, "orna query");
    let report: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("orna query prints JSON");
    report["rows"].as_u64().expect("the report counts rows")
}

pub fn head(root: &Path) -> String {
    git(root, &["rev-parse", "HEAD"]).trim().to_owned()
}

pub fn relation_bytes(relation_hex: &str) -> [u8; 16] {
    let bytes = (0..relation_hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&relation_hex[at..at + 2], 16).expect("hex relation id"))
        .collect::<Vec<_>>();
    bytes.try_into().expect("a relation id is 16 bytes")
}

/// One committed row as the store holds it: its key and its stored fields
/// by field name, decoded under the descriptor committed at `commit`.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredRow {
    pub key: TypedKey,
    pub fields: BTreeMap<String, Raw>,
}

/// Every committed row of `relation` at `commit`, read through the same
/// format-3 row map and native graph the query and history verbs use.
pub fn stored_rows(root: &Path, commit: &str, relation: [u8; 16]) -> Vec<StoredRow> {
    let repository = Repository::discover(root).expect("discover the repository");
    let format = repository
        .open_pinned_format_context(commit)
        .expect("open the committed format context");
    let row_map = format
        .load_row_map(relation)
        .expect("load the relation row map");
    let graph = format
        .open_native_graph(&row_map)
        .expect("open the native graph");
    let scope = graph.open_read_scope().expect("open a read scope");
    let descriptor = graph
        .read_schema_descriptor(&scope)
        .expect("read the committed descriptor");
    let Raw::Map(entries) = descriptor.raw() else {
        panic!("a descriptor is a map");
    };
    let Some((_, Raw::Array(declared))) =
        entries.iter().find(|(key, _)| *key == Raw::Int(3.into()))
    else {
        panic!("a descriptor lists its fields");
    };
    let stored_names = declared
        .iter()
        .filter_map(|field| match field {
            Raw::Array(parts) if parts.get(3) == Some(&Raw::Int(1.into())) => match &parts[1] {
                Raw::Text(name) => Some(name.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    graph
        .range_rows(&KeyRange::new(None, None, 64).expect("key range"), &scope)
        .expect("read the committed rows")
        .into_iter()
        .map(|row| {
            let Raw::Array(values) = decode_canonical_storage_value(
                row.value().encoded_fields().expect("an inline stored row"),
            )
            .expect("canonical stored fields") else {
                panic!("stored fields are an array");
            };
            assert_eq!(values.len(), stored_names.len());
            StoredRow {
                key: row.key().clone(),
                fields: stored_names.iter().cloned().zip(values).collect(),
            }
        })
        .collect()
}

/// The allocator watermark of each automatic-key table `commit` recorded in
/// its store (ORNA-AUTOID-005), as decimal text by table relation id hex;
/// `None` when the snapshot holds no allocator summary.
pub fn committed_watermarks(root: &Path, commit: &str) -> Option<BTreeMap<String, String>> {
    let repository = Repository::discover(root).expect("discover the repository");
    let commit = repository
        .resolve_snapshot(commit)
        .expect("resolve the commit");
    repository
        .read_committed_allocator_watermarks(&commit)
        .expect("read the allocator summary")
        .map(|watermarks| {
            watermarks
                .into_iter()
                .map(|(table, watermark)| {
                    (
                        orna_repository_v1::Uuid::from_bytes(table)
                            .simple()
                            .to_string(),
                        watermark.to_string(),
                    )
                })
                .collect()
        })
}

pub fn text(value: &str) -> Raw {
    Raw::Text(value.to_owned())
}

pub fn option(value: Option<Raw>) -> Raw {
    Raw::Tag(
        60013,
        Box::new(Raw::Array(match value {
            Some(value) => vec![Raw::Int(1.into()), value],
            None => vec![Raw::Int(0.into())],
        })),
    )
}

pub fn decimal(coefficient: i64, exponent: i64) -> Raw {
    Raw::Tag(
        60000,
        Box::new(Raw::Array(vec![
            Raw::Int(coefficient.into()),
            Raw::Int(exponent.into()),
        ])),
    )
}
