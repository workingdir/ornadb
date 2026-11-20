use super::*;

/// ORNA-MUT-008 through the live host (ornadb-pl0oi): an Eval that deletes a
/// row a committed vehicle still references is refused fail-closed. The socket
/// closes with 1002 and commits nothing, so the referencing row and its owner
/// stay committed. The project declares no table assertions, so the reference
/// check must hold without one. Answering that refusal with its
/// `ORNA-REF-DANGLING` diagnostic needs a committed-bound reference snapshot
/// (follow-up bead), so this test does not claim it. A fresh session then
/// commits as usual.
#[test]
fn served_eval_deleting_a_referenced_row_is_refused_and_commits_nothing() {
    let directory = tempfile::tempdir().expect("temporary project");
    orna_repository::initialize_repository(directory.path()).expect("orna init");
    std::fs::write(
        directory.path().join("main.orna"),
        "pub table Owner(id: Str) { name: Str, }\n\
         pub table Vehicle(id: Str) { owner: Owner, }\n",
    )
    .expect("project source");
    commit_project_with_identities(directory.path());
    let endpoint = Endpoint::Path(
        directory
            .path()
            .to_str()
            .expect("UTF-8 project path")
            .to_owned(),
    );
    let clone = open_served_clone(&endpoint).expect("`orna serve` resolves the clone");
    let database = clone.identity.database_id;
    let (address, _state) = serve_clone(clone, ServeOptions::default());
    let session = create_session_in(address, database);
    let mut socket = open_websocket(address, &session);

    // The vehicle holds a reference to the owner it inserts in the same
    // activation, as the REPL does; a `Str` key is not a reference value.
    let linked = eval_over_socket(
        &mut socket,
        &session,
        [51; 16],
        r#"main.Vehicle.insert({ id: "v1", owner: main.Owner.insert({ id: "ada", name: "Ada" }) })"#,
    );
    assert_eq!(linked.request, Some([51; 16]), "linked write is correlated");
    assert_eq!(
        committed_rows(directory.path(), "Owner").len(),
        1,
        "the owner commits with the vehicle that references it"
    );
    assert_eq!(
        committed_rows(directory.path(), "Vehicle").len(),
        1,
        "the vehicle commits and holds a reference to its owner"
    );

    let delete = eval_envelope(&session, [52; 16], r#"main.Owner.delete("ada")"#);
    send_request_over_socket(&mut socket, &session, delete);
    expect_server_close(&mut socket, 1002);
    assert_eq!(
        committed_rows(directory.path(), "Owner").len(),
        1,
        "a refused delete keeps its owner"
    );
    assert_eq!(
        committed_rows(directory.path(), "Vehicle").len(),
        1,
        "a refused delete keeps the referencing vehicle"
    );

    let resumed = create_session_in(address, database);
    let mut resumed_socket = open_websocket(address, &resumed);
    let row = CanonicalValue::new(OvbRaw::Map(vec![
        (OvbRaw::Text("id".into()), OvbRaw::Text("bob".into())),
        (OvbRaw::Text("name".into()), OvbRaw::Text("Bob".into())),
    ]))
    .expect("canonical row");
    let admitted = eval_over_socket(
        &mut resumed_socket,
        &resumed,
        [53; 16],
        r#"main.Owner.insert({ id: "bob", name: "Bob" })"#,
    );
    assert_success_value(&admitted, &row);
    assert_eq!(
        committed_rows(directory.path(), "Owner").len(),
        2,
        "the admitted write commits exactly one more row"
    );
}
