#![cfg(feature = "system-host-bindings")]

//! `sys.git.branches` evaluated over a real repository the host attached: one
//! `sys.Branch` row per local branch, every reference pinned to the commit the
//! host attached at, and `sys.snapshot.incomplete` (never a fabricated row) when
//! the repository or the pin is unavailable (ORNA-SYS-111, ORNA-SYS-112).

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use orna_evaluator::{
    Environment, EvaluationError, Functions, Limits, NominalDefinitions, PureFunction,
    SysHostContext, invoke_named_with_sys_host_context,
};
use orna_syntax::parse_expression;
use orna_sys::{RuntimeMode, StdSnapshotPin};
use orna_value::{
    CanonicalSnapshot, CanonicalValue, GitHash, GitObjectId, OvbRaw, branch_reference,
    commit_reference,
};
use tempfile::TempDir;

const DATABASE: [u8; 16] = [7; 16];

/// Runs `git` in `directory` under a fixed identity and returns its trimmed
/// standard output.
fn git_with_input(directory: &Path, arguments: &[&str], input: &str) -> String {
    let mut child = Command::new("git")
        .current_dir(directory)
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

fn git(directory: &Path, arguments: &[&str]) -> String {
    git_with_input(directory, arguments, "")
}

/// A repository on `main`, with one empty commit.
fn repository() -> TempDir {
    let directory = tempfile::tempdir().expect("repository directory");
    git(directory.path(), &["init", "-q", "-b", "main"]);
    git(
        directory.path(),
        &["commit", "-q", "--allow-empty", "-m", "base"],
    );
    directory
}

/// Creates `name` at a commit of its own, a child of `HEAD`, without moving
/// `HEAD`.
fn branch_at_new_commit(root: &Path, name: &str) -> String {
    let commit = git(
        root,
        &["commit-tree", "HEAD^{tree}", "-p", "HEAD", "-m", name],
    );
    git(
        root,
        &["update-ref", &format!("refs/heads/{name}"), &commit],
    );
    commit
}

fn oid_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("a hexadecimal object id"))
        .collect()
}

/// The commit pin of a host attached when `HEAD` named `head`.
fn pin(head: &str) -> CanonicalSnapshot {
    CanonicalSnapshot::Commit {
        database: DATABASE,
        algorithm: GitHash::Sha1,
        oid: oid_bytes(head),
    }
}

fn context(root: &Path, head: Option<&str>) -> SysHostContext {
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
fn attached(root: &Path) -> SysHostContext {
    context(root, Some(&git(root, &["rev-parse", "HEAD"])))
}

fn evaluate(expression: &str, host: &SysHostContext) -> Result<CanonicalValue, EvaluationError> {
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

fn value(expression: &str, host: &SysHostContext) -> OvbRaw {
    evaluate(expression, host)
        .unwrap_or_else(|error| panic!("{expression}: {}", error.code()))
        .raw()
        .clone()
}

/// The payload of an optional value read through `first()` or `last()`.
fn some(option: OvbRaw) -> OvbRaw {
    let OvbRaw::Tag(_, inner) = option else {
        panic!("an optional value is a tagged option");
    };
    let OvbRaw::Array(mut parts) = *inner else {
        panic!("an option holds its presence and payload");
    };
    assert_eq!(parts.len(), 2, "a present option: {parts:?}");
    parts.pop().expect("a payload")
}

fn int(value: i64) -> OvbRaw {
    OvbRaw::Int(value.into())
}

fn reference(canonical: CanonicalValue) -> OvbRaw {
    canonical.raw().clone()
}

fn branch_ref(head: &str, name: &str) -> OvbRaw {
    reference(
        branch_reference(DATABASE, pin(head), name)
            .expect("a branch reference")
            .to_canonical()
            .expect("it encodes"),
    )
}

fn commit_ref(head: &str, commit: &str) -> OvbRaw {
    let oid = GitObjectId::new(GitHash::Sha1, oid_bytes(commit)).expect("an object id");
    reference(
        commit_reference(DATABASE, pin(head), oid)
            .expect("a commit reference")
            .to_canonical()
            .expect("it encodes"),
    )
}

// Specified: 34-system-reference.md `sys.Branch`; ORNA-SYS-112. A repository with
// two branches yields two rows with distinct commit references.
#[test]
fn two_branches_are_two_rows_with_distinct_commit_references() {
    let directory = repository();
    let root = directory.path();
    let main_tip = git(root, &["rev-parse", "main"]);
    let topic_tip = branch_at_new_commit(root, "topic");
    assert_ne!(main_tip, topic_tip);
    let host = attached(root);

    assert_eq!(value("sys.git.branches | count()", &host), int(2));
    assert_eq!(
        some(value(
            "sys.git.branches | map(b => b.name) | first()",
            &host
        )),
        OvbRaw::Text("main".to_owned())
    );
    assert_eq!(
        some(value("sys.git.branches | map(b => b.name) | last()", &host)),
        OvbRaw::Text("topic".to_owned())
    );

    let first_commit = some(value(
        "sys.git.branches | map(b => b.commit) | first()",
        &host,
    ));
    let last_commit = some(value(
        "sys.git.branches | map(b => b.commit) | last()",
        &host,
    ));
    assert_eq!(first_commit, commit_ref(&main_tip, &main_tip));
    assert_eq!(last_commit, commit_ref(&main_tip, &topic_tip));
    assert_ne!(first_commit, last_commit, "distinct commit references");

    assert_eq!(
        some(value(
            "sys.git.branches | map(b => b.reference) | first()",
            &host
        )),
        branch_ref(&main_tip, "main")
    );
    assert_eq!(
        some(value(
            "sys.git.branches | map(b => b.reference) | last()",
            &host
        )),
        branch_ref(&main_tip, "topic")
    );
}

#[test]
fn the_branch_head_is_attached_to_is_current() {
    let directory = repository();
    let root = directory.path();
    branch_at_new_commit(root, "topic");
    let host = attached(root);
    let current = "sys.git.branches | filter(b => b.current) | map(b => b.name)";
    assert_eq!(
        some(value(&format!("{current} | first()"), &host)),
        OvbRaw::Text("main".to_owned())
    );
    assert_eq!(value(&format!("{current} | count()"), &host), int(1));

    git(root, &["checkout", "-q", "topic"]);
    assert_eq!(
        some(value(&format!("{current} | first()"), &host)),
        OvbRaw::Text("topic".to_owned())
    );

    git(root, &["checkout", "-q", "--detach"]);
    assert_eq!(
        value(&format!("{current} | count()"), &host),
        int(0),
        "a detached HEAD is attached to no branch"
    );
}

// Specified: `ahead` and `behind` are null only when there is no upstream.
#[test]
fn ahead_and_behind_are_counted_against_the_upstream_and_null_without_one() {
    let directory = repository();
    let root = directory.path();
    git(root, &["branch", "topic"]);
    git(root, &["branch", "--set-upstream-to=main", "topic"]);
    git(root, &["checkout", "-q", "topic"]);
    git(root, &["commit", "-q", "--allow-empty", "-m", "topic one"]);
    git(root, &["checkout", "-q", "main"]);
    git(root, &["commit", "-q", "--allow-empty", "-m", "main one"]);
    git(root, &["commit", "-q", "--allow-empty", "-m", "main two"]);
    let host = attached(root);

    let counts = |name: &str| {
        format!(
            "sys.git.branches | filter(b => b.name == \"{name}\") | map(b => (b.ahead ?? -1) * 10 + (b.behind ?? -1)) | first()"
        )
    };
    assert_eq!(some(value(&counts("topic"), &host)), int(12));
    assert_eq!(some(value(&counts("main"), &host)), int(-11));

    // The evaluator does not type-check, so the default may be of another type.
    let upstream = |name: &str| {
        format!(
            "sys.git.branches | filter(b => b.name == \"{name}\") | map(b => b.upstream ?? \"none\") | first()"
        )
    };
    let main_tip = git(root, &["rev-parse", "main"]);
    assert_eq!(
        some(value(&upstream("topic"), &host)),
        reference(
            orna_value::git_ref_reference(DATABASE, pin(&main_tip), "refs/heads/main")
                .expect("a ref reference")
                .to_canonical()
                .expect("it encodes")
        ),
        "topic names the ref it tracks"
    );
    assert_eq!(
        some(value(&upstream("main"), &host)),
        OvbRaw::Text("none".to_owned()),
        "main tracks nothing, so its upstream is null"
    );
}

// Specified: ORNA-SYS-111; no row is served over a pin that is not there.
#[test]
fn a_host_with_no_commit_pin_serves_no_branch() {
    let directory = repository();
    let host = context(directory.path(), None);
    let failure = evaluate("sys.git.branches | count()", &host)
        .expect_err("a branch cannot be pinned without a commit");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

#[test]
fn a_repository_with_no_branch_needs_no_pin() {
    let directory = tempfile::tempdir().expect("repository directory");
    git(directory.path(), &["init", "-q", "-b", "main"]);
    let host = context(directory.path(), None);
    assert_eq!(value("sys.git.branches | count()", &host), int(0));
}

#[test]
fn a_root_that_is_not_a_repository_fails_closed() {
    let directory = tempfile::tempdir().expect("plain directory");
    let host = context(directory.path(), Some(&"ab".repeat(20)));
    let failure = evaluate("sys.git.branches | count()", &host)
        .expect_err("no repository is attached at the root");
    assert_eq!(failure.code(), "sys.snapshot.incomplete");
}

#[test]
fn a_large_branch_set_is_read_across_pages_without_repeating_or_skipping() {
    let directory = repository();
    let root = directory.path();
    let tip = git(root, &["rev-parse", "main"]);
    let updates = (0..600)
        .map(|index| format!("create refs/heads/bulk/{index:04} {tip}\n"))
        .collect::<String>();
    git_with_input(root, &["update-ref", "--stdin"], &updates);
    let host = attached(root);

    assert_eq!(value("sys.git.branches | count()", &host), int(601));
    assert_eq!(
        some(value("sys.git.branches | map(b => b.name) | last()", &host)),
        OvbRaw::Text("main".to_owned()),
        "`bulk/…` sorts before `main`, which is the last of the 601"
    );
    // The pages break after the 256th and 512th rows: the branches either
    // side of each break are each served exactly once.
    for index in [0, 255, 256, 257, 511, 512, 513, 599] {
        let name = format!("bulk/{index:04}");
        assert_eq!(
            value(
                &format!("sys.git.branches | filter(b => b.name == \"{name}\") | count()"),
                &host
            ),
            int(1),
            "{name}"
        );
    }
}
