//! What the evaluator tests of the `sys.git.*` members share: a real repository,
//! the host a project run attaches over it, and the evaluation of one expression
//! against that host.

// Included by path from several test targets, each of which uses the part of
// the support surface it needs.
#![allow(dead_code)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use orna_evaluator::{
    Environment, EvaluationError, Functions, Limits, NominalDefinitions, PureFunction,
    SysHostContext, invoke_named_with_sys_host_context,
};
use orna_syntax::parse_expression;
use orna_sys::{RuntimeMode, StdSnapshotPin};
use orna_value::{CanonicalSnapshot, CanonicalValue, GitHash, OvbRaw};
use tempfile::TempDir;

pub const DATABASE: [u8; 16] = [7; 16];

/// Runs `git` in `directory` under a fixed identity with `input` on its standard
/// input and returns its trimmed standard output.
pub fn git_with_input(directory: &Path, arguments: &[&str], input: &str) -> String {
    git_with_environment(directory, &[], arguments, input)
}

/// Runs `git` in `directory` under a fixed identity, with `environment` set for it and
/// `input` on its standard input; returns its trimmed standard output.
pub fn git_with_environment(
    directory: &Path,
    environment: &[(&str, &str)],
    arguments: &[&str],
    input: &str,
) -> String {
    let mut child = Command::new("git")
        .current_dir(directory)
        .envs(environment.iter().copied())
        .args([
            "-c",
            "user.name=orna-test",
            "-c",
            "user.email=orna-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("git starts");
    child
        .stdin
        .take()
        .expect("git stdin")
        .write_all(input.as_bytes())
        .expect("write git stdin");
    let output = child.wait_with_output().expect("git exits");
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

pub fn git(directory: &Path, arguments: &[&str]) -> String {
    git_with_input(directory, arguments, "")
}

/// A repository on `main`, with one empty commit.
pub fn repository() -> TempDir {
    let directory = tempfile::tempdir().expect("repository directory");
    git(directory.path(), &["init", "-q", "-b", "main"]);
    git(
        directory.path(),
        &["commit", "-q", "--allow-empty", "-m", "base"],
    );
    directory
}

pub fn oid_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("a hexadecimal object id"))
        .collect()
}

/// The commit pin of a host attached when `HEAD` named `head`.
pub fn pin(head: &str) -> CanonicalSnapshot {
    CanonicalSnapshot::Commit {
        database: DATABASE,
        algorithm: GitHash::Sha1,
        oid: oid_bytes(head),
    }
}

pub fn context(root: &Path, head: Option<&str>) -> SysHostContext {
    SysHostContext {
        database_id: DATABASE,
        repository_id: [2; 16],
        runtime_id: [0x11; 16],
        root: root.to_path_buf(),
        read_only: false,
        mode: RuntimeMode::Run,
        repository_layout_version: "3".to_owned(),
        storage_manifest_version: "store-3".to_owned(),
        std_snapshot: StdSnapshotPin::Unpinned,
        head_commit: head.map(oid_bytes),
    }
}

/// The host a project run attaches: `HEAD` of `root` is the pin.
pub fn attached(root: &Path) -> SysHostContext {
    context(root, Some(&git(root, &["rev-parse", "HEAD"])))
}

pub fn evaluate(
    expression: &str,
    host: &SysHostContext,
) -> Result<CanonicalValue, EvaluationError> {
    let parsed = parse_expression(expression);
    assert!(parsed.is_ok(), "{expression}: {:?}", parsed.diagnostics);
    let functions = Functions::from([(
        "probe".to_owned(),
        PureFunction {
            parameters: Vec::new(),
            body: parsed.value,
            environment: Environment::new(),
        },
    )]);
    invoke_named_with_sys_host_context(
        "probe",
        &functions,
        &Environment::new(),
        &NominalDefinitions::new(),
        Limits::default(),
        host,
    )
}

pub fn value(expression: &str, host: &SysHostContext) -> OvbRaw {
    evaluate(expression, host)
        .unwrap_or_else(|error| panic!("{expression}: {}", error.code()))
        .raw()
        .clone()
}

/// The payload of an optional value read through `first()` or `last()`.
pub fn some(option: OvbRaw) -> OvbRaw {
    let OvbRaw::Tag(_, inner) = option else {
        panic!("an optional value is a tagged option");
    };
    let OvbRaw::Array(mut parts) = *inner else {
        panic!("an option holds its presence and payload");
    };
    assert_eq!(parts.len(), 2, "a present option: {parts:?}");
    parts.pop().expect("a payload")
}

/// Whether an optional value is absent.
pub fn is_absent(option: &OvbRaw) -> bool {
    let OvbRaw::Tag(_, inner) = option else {
        panic!("an optional value is a tagged option");
    };
    let OvbRaw::Array(parts) = inner.as_ref() else {
        panic!("an option holds its presence and payload");
    };
    match parts.as_slice() {
        [absent] => *absent == OvbRaw::Int(0.into()),
        [present, _] if *present == OvbRaw::Int(1.into()) => false,
        other => panic!("not an option: {other:?}"),
    }
}

pub fn int(value: i64) -> OvbRaw {
    OvbRaw::Int(value.into())
}

pub fn text(value: &str) -> OvbRaw {
    OvbRaw::Text(value.to_owned())
}

pub fn reference(canonical: CanonicalValue) -> OvbRaw {
    canonical.raw().clone()
}

/// The field `name` of a record value.
pub fn field<'a>(record: &'a OvbRaw, name: &str) -> &'a OvbRaw {
    let OvbRaw::Map(entries) = record else {
        panic!("a record is a map: {record:?}");
    };
    entries
        .iter()
        .find_map(|(key, value)| matches!(key, OvbRaw::Text(text) if text == name).then_some(value))
        .unwrap_or_else(|| panic!("the record has no `{name}` field: {record:?}"))
}
