use super::*;

/// Commits a plain project with one table, serves it, and opens one socket in a
/// session on it. The project declares no stored reference and no table
/// assertion, so the served host alone binds the table's committed rows.
fn serve_music(directory: &Path) -> (Arc<ServeState>, LiveSession, TcpStream) {
    orna_repository::initialize_repository(directory).expect("orna init");
    std::fs::write(
        directory.join("main.orna"),
        "pub table music(id: Str) { title: Str, artist: Str, }\n",
    )
    .expect("project source");
    commit_project_with_identities(directory);
    let endpoint = Endpoint::Path(directory.to_str().expect("UTF-8 project path").to_owned());
    let clone = open_served_clone(&endpoint).expect("`orna serve` resolves the clone");
    let database = clone.identity.database_id;
    let (address, state) = serve_clone(clone, ServeOptions::default());
    let session = create_session_in(address, database);
    let socket = open_websocket(address, &session);
    (state, session, socket)
}

/// Asserts that `response` is a success result, whatever value it carries.
fn assert_success(response: &Envelope) {
    let Message::Result {
        status, diagnostic, ..
    } = &response.message
    else {
        panic!("Eval answers with a Result, not {response:?}");
    };
    assert_eq!(
        *status,
        orna_protocol::ResultStatus::Success,
        "{diagnostic:?}"
    );
}

/// ORNA-EVAL-002 through the live host (ornadb-nb5yj): a served Eval reads a
/// plain table's committed rows at the pinned HEAD, as the local REPL does. A
/// later Eval updates the committed row, and a read sees the update through the
/// same table snapshot.
#[test]
fn served_eval_reads_and_updates_the_committed_rows_of_a_plain_table() {
    let directory = tempfile::tempdir().expect("temporary project");
    let (_state, session, mut socket) = serve_music(directory.path());

    let inserted = eval_over_socket(
        &mut socket,
        &session,
        [81; 16],
        r#"main.music.insert({ id: "a", title: "Kai", artist: "Kai" })"#,
    );
    assert_success(&inserted);
    assert_eq!(
        committed_rows(directory.path(), "music").len(),
        1,
        "the insert commits its row"
    );

    let updated = eval_over_socket(
        &mut socket,
        &session,
        [82; 16],
        r#"main.music.update("a", { title: "Ash", artist: "Kai" })"#,
    );
    assert_success(&updated);
    assert_eq!(
        committed_rows(directory.path(), "music").len(),
        1,
        "the update keeps one committed row"
    );

    assert_success_value(
        &eval_over_socket(
            &mut socket,
            &session,
            [83; 16],
            r#"main.music | filter(m => m.title == "Ash") | count()"#,
        ),
        &int_value(1),
    );
}
