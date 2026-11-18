//! ORNA-SERVE-003, ORNA-SYS-011 (ornadb-5il1.276, ornadb-jzanz): a live Eval is
//! served at the commit `HEAD` names when it runs. The server outlives every
//! commit it serves, so each Eval binds the commit it observes and the identities
//! committed there, and reads the Git state, the catalogue and the runtime facts
//! of the clone through the live protocol.

use super::*;

/// Starts the served clone at `root` and opens a live session on a WebSocket.
/// The serve state is returned so the server outlives the test body.
fn serve_session(root: &Path) -> (TcpStream, LiveSession, Arc<ServeState>) {
    let endpoint = Endpoint::Path(root.to_str().expect("UTF-8 project path").to_owned());
    let clone = open_served_clone(&endpoint).expect("`orna serve` resolves the clone");
    let database = clone.identity.database_id;
    let (address, state) = serve_clone(clone, ServeOptions::default());
    let session = create_session_in(address, database);
    let socket = open_websocket(address, &session);
    (socket, session, state)
}

/// A clone that declares one table, committed with its identities.
fn committed_note_clone() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temporary project");
    orna_repository::initialize_repository(directory.path()).expect("orna init");
    std::fs::write(
        directory.path().join("main.orna"),
        "pub table Note(id: Int) { text: Str, }\n",
    )
    .expect("project source");
    commit_project_with_identities(directory.path());
    directory
}

/// The raw object id of the commit `HEAD` names in `root`.
fn head_object_id(root: &Path) -> Vec<u8> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .expect("git rev-parse starts");
    assert!(output.status.success(), "git rev-parse HEAD succeeds");
    let hex = String::from_utf8(output.stdout).expect("UTF-8 object id");
    let hex = hex.trim();
    (0..hex.len())
        .step_by(2)
        .map(|start| u8::from_str_radix(&hex[start..start + 2], 16).expect("hex object id"))
        .collect()
}

/// Commits nothing but a new commit, so `HEAD` moves.
fn commit_empty(root: &Path, message: &str) {
    git_succeeds(
        root,
        &[
            "-c",
            "user.name=orna-test",
            "-c",
            "user.email=orna-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            message,
        ],
    );
}

/// The canonical encoding of the value a success `response` carries.
fn result_bytes(response: &Envelope) -> Vec<u8> {
    let Message::Result {
        value: Some(value), ..
    } = &response.message
    else {
        panic!("Eval answers with a Result carrying a value, not {response:?}");
    };
    value.encode().expect("a canonical value encodes")
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// An Eval reads the Git state, the catalogue and the runtime facts of the clone
/// it serves. Before, the host pinned no commit and refused each of these reads.
#[test]
fn served_eval_reads_the_system_relations_of_the_clone() {
    let directory = committed_note_clone();
    let (mut socket, session, _state) = serve_session(directory.path());
    let reads = [
        ("sys.git.branches | count()", int_value(1)),
        ("sys.catalog.tables | count()", int_value(1)),
        ("sys.catalog.columns | count()", int_value(2)),
        (
            "sys.rt.info().mode == sys.RuntimeMode.serve",
            CanonicalValue::new(OvbRaw::Bool(true)).expect("canonical boolean"),
        ),
    ];
    for (index, (source, expected)) in reads.iter().enumerate() {
        let request = [70 + u8::try_from(index).expect("few reads"); 16];
        let answered = eval_over_socket(&mut socket, &session, request, source);
        assert_success_value(&answered, expected);
    }
}

/// Every reference an Eval reads is pinned to the commit `HEAD` names when that
/// Eval runs (ORNA-SYS-011, ORNA-SYS-019): after a new commit the next Eval's
/// reference carries the new commit, and no longer the one `HEAD` named before.
#[test]
fn served_eval_pins_its_references_to_the_head_it_observes() {
    let directory = committed_note_clone();
    git_succeeds(directory.path(), &["branch", "topic"]);
    let first_head = head_object_id(directory.path());
    let (mut socket, session, _state) = serve_session(directory.path());
    let topic_reference = r#"sys.git.branches | filter(branch => branch.name == "topic") | map(branch => branch.reference) | first()"#;

    let first = result_bytes(&eval_over_socket(
        &mut socket,
        &session,
        [80; 16],
        topic_reference,
    ));
    assert!(
        contains_bytes(&first, &first_head),
        "the reference is pinned to the commit HEAD names"
    );

    commit_empty(directory.path(), "second");
    let second_head = head_object_id(directory.path());
    assert_ne!(first_head, second_head, "HEAD moved to a new commit");
    let second = result_bytes(&eval_over_socket(
        &mut socket,
        &session,
        [81; 16],
        topic_reference,
    ));
    assert!(
        contains_bytes(&second, &second_head),
        "the next Eval is pinned to the new HEAD"
    );
    assert!(
        !contains_bytes(&second, &first_head),
        "and no longer to the commit HEAD named before"
    );
}

/// A clone whose `HEAD` names no commit has nothing to pin its references to:
/// the Eval that counted its branches one request earlier is refused and serves
/// no row (ORNA-SYS-111), so the pin is what that read needs.
#[test]
fn served_eval_of_a_clone_with_no_commit_reads_no_branches() {
    let directory = committed_note_clone();
    let (mut socket, session, _state) = serve_session(directory.path());
    let count = "sys.git.branches | count()";
    assert_success_value(
        &eval_over_socket(&mut socket, &session, [90; 16], count),
        &int_value(1),
    );

    // `HEAD` now names a branch with no commit, while the committed branch exists.
    git_succeeds(directory.path(), &["checkout", "-q", "--orphan", "unborn"]);
    send_request_over_socket(
        &mut socket,
        &session,
        eval_envelope(&session, [91; 16], count),
    );
    expect_server_close(&mut socket, 1002);
}
