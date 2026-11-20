use super::*;

/// Commits the garage project, where `Vehicle.owner` is a stored reference to
/// `Owner`, serves it, and opens one socket in a session on it. The project
/// declares no table assertions, so each reference check must hold without one.
fn serve_garage(directory: &Path) -> (Arc<ServeState>, LiveSession, TcpStream) {
    orna_repository::initialize_repository(directory).expect("orna init");
    std::fs::write(
        directory.join("main.orna"),
        "pub table Owner(id: Str) { name: Str, }\n\
         pub table Vehicle(id: Str) { owner: Owner, }\n",
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

/// Inserts owner `ada` and vehicle `v1`, which references it, in one activation
/// as the REPL does, and asserts that both commit. A `Str` key is not a
/// reference value, so the vehicle holds the owner the insert returns.
fn commit_ada_with_vehicle(
    directory: &Path,
    socket: &mut TcpStream,
    session: &LiveSession,
    request: [u8; 16],
) {
    let linked = eval_over_socket(
        socket,
        session,
        request,
        r#"main.Vehicle.insert({ id: "v1", owner: main.Owner.insert({ id: "ada", name: "Ada" }) })"#,
    );
    assert_eq!(linked.request, Some(request), "linked write is correlated");
    assert_eq!(
        committed_rows(directory, "Owner").len(),
        1,
        "the owner commits with the vehicle that references it"
    );
    assert_eq!(
        committed_rows(directory, "Vehicle").len(),
        1,
        "the vehicle commits and holds a reference to its owner"
    );
}

/// ORNA-MUT-008 through the live host (ornadb-zvz7v): an Eval that deletes a
/// row a committed vehicle still references is refused with a correlated
/// `ORNA-REF-DANGLING` diagnostic, and the socket stays open: a well-formed
/// request that a commit refuses is answered, not closed (30-protocol). The
/// refusal commits nothing, so the referencing row and its owner stay
/// committed. The project declares no table assertions, so the reference check
/// must hold without one.
#[test]
fn served_eval_deleting_a_referenced_row_is_refused_with_its_diagnostic() {
    let directory = tempfile::tempdir().expect("temporary project");
    let (_state, session, mut socket) = serve_garage(directory.path());
    commit_ada_with_vehicle(directory.path(), &mut socket, &session, [51; 16]);

    let delete = eval_over_socket(
        &mut socket,
        &session,
        [52; 16],
        r#"main.Owner.delete("ada")"#,
    );
    let Message::Diagnostic { diagnostic, .. } = &delete.message else {
        panic!("Eval answers with a Diagnostic, not {delete:?}");
    };
    assert_eq!(delete.request, Some([52; 16]), "the refusal is correlated");
    assert_eq!(
        diagnostic.code().expect("a portable diagnostic code"),
        "ORNA-REF-DANGLING"
    );
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

    let row = CanonicalValue::new(OvbRaw::Map(vec![
        (OvbRaw::Text("id".into()), OvbRaw::Text("bob".into())),
        (OvbRaw::Text("name".into()), OvbRaw::Text("Bob".into())),
    ]))
    .expect("canonical row");
    let admitted = eval_over_socket(
        &mut socket,
        &session,
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

/// ORNA-REF-003 through the live host (ornadb-o4rl0): an Eval that re-keys a row
/// a committed vehicle still references is refused with a correlated
/// `ORNA-REF-DANGLING` diagnostic, and the socket stays open. The refusal
/// commits nothing, so the owner keeps its key and the vehicle keeps its
/// reference.
#[test]
fn served_eval_rekeying_a_referenced_row_is_refused_with_its_diagnostic() {
    let directory = tempfile::tempdir().expect("temporary project");
    let (_state, session, mut socket) = serve_garage(directory.path());
    commit_ada_with_vehicle(directory.path(), &mut socket, &session, [61; 16]);
    let owners = committed_rows(directory.path(), "Owner");
    let vehicles = committed_rows(directory.path(), "Vehicle");

    let rekey = eval_over_socket(
        &mut socket,
        &session,
        [62; 16],
        r#"main.Owner.rekey("ada", "ada2")"#,
    );
    let Message::Diagnostic { diagnostic, .. } = &rekey.message else {
        panic!("Eval answers with a Diagnostic, not {rekey:?}");
    };
    assert_eq!(rekey.request, Some([62; 16]), "the refusal is correlated");
    assert_eq!(
        diagnostic.code().expect("a portable diagnostic code"),
        "ORNA-REF-DANGLING"
    );
    assert_eq!(
        committed_rows(directory.path(), "Owner"),
        owners,
        "a refused re-key keeps the owner under its key"
    );
    assert_eq!(
        committed_rows(directory.path(), "Vehicle"),
        vehicles,
        "a refused re-key keeps the referencing vehicle"
    );

    let row = CanonicalValue::new(OvbRaw::Map(vec![
        (OvbRaw::Text("id".into()), OvbRaw::Text("bob".into())),
        (OvbRaw::Text("name".into()), OvbRaw::Text("Bob".into())),
    ]))
    .expect("canonical row");
    let admitted = eval_over_socket(
        &mut socket,
        &session,
        [63; 16],
        r#"main.Owner.insert({ id: "bob", name: "Bob" })"#,
    );
    assert_success_value(&admitted, &row);
}

/// ORNA-MUT-007 through the live host (ornadb-o4rl0): the constraint is judged
/// at activation commit, so one served Eval may re-key a referenced row and
/// update the vehicle that references it in the same activation. Both commit,
/// the owner moves to its new key rather than being copied, and the vehicle
/// reads through the re-keyed owner.
#[test]
fn served_eval_rekeying_a_referenced_row_with_its_dependent_in_one_activation_is_accepted() {
    let directory = tempfile::tempdir().expect("temporary project");
    let (_state, session, mut socket) = serve_garage(directory.path());
    commit_ada_with_vehicle(directory.path(), &mut socket, &session, [71; 16]);

    let moved = eval_over_socket(
        &mut socket,
        &session,
        [72; 16],
        r#"main.Vehicle.update("v1", { owner: main.Owner.rekey("ada", "ada2") })"#,
    );
    assert_eq!(moved.request, Some([72; 16]), "the move is correlated");
    assert!(
        !matches!(moved.message, Message::Diagnostic { .. }),
        "the re-key and its dependent update are accepted, not {moved:?}"
    );
    assert_success_value(
        &eval_over_socket(
            &mut socket,
            &session,
            [73; 16],
            r#"main.Owner | filter(o => o.id == "ada2") | count()"#,
        ),
        &int_value(1),
    );
    assert_success_value(
        &eval_over_socket(
            &mut socket,
            &session,
            [74; 16],
            r#"main.Vehicle | filter(v => v.owner.name == "Ada") | count()"#,
        ),
        &int_value(1),
    );
    assert_eq!(
        committed_rows(directory.path(), "Owner").len(),
        1,
        "the owner moves to its new key rather than being copied"
    );
    assert_eq!(
        committed_rows(directory.path(), "Vehicle").len(),
        1,
        "the vehicle still exists"
    );
}
