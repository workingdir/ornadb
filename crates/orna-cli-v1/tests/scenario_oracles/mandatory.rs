//! Entries for the retained oracles behind the §39 mandatory behavioural
//! scenarios.

use std::{
    net::TcpStream,
    path::Path,
    process::Child,
    thread,
    time::{Duration, Instant},
};

use tungstenite::{Message, client::IntoClientRequest, http::HeaderValue};

use crate::harness::{
    Check, Db, Entry, Failure, Probe, check_refuses, ensure, repl_refuses, unknown_command,
};

pub const LIVE_BLOCKER: &str = "`orna serve` keeps no live watch current: a watch over a table-derived value is refused, or gets no snapshot or delta after the table changes";
const SKIP_BLOCKER: &str = "an operator cannot skip a failed delivery: `sys.Failure` does not evaluate in the project REPL";

pub fn entries() -> Vec<Entry> {
    vec![
        Entry::gap(
            "STATE-001",
            "ornadb-5il1.135",
            "the project REPL cannot read a table, so neither the CWD relation nor its `.as_of(HEAD)` view can be queried",
            repl_cannot_read_a_table,
        ),
        Entry::gap(
            "KEY-001",
            "ornadb-5il1.121",
            "the format-3 automatic-ID allocator issues no second ID: a second insert into a key-less table is refused",
            second_automatic_id_is_refused,
        ),
        Entry::gap(
            "KEY-002",
            "ornadb-5il1.121",
            "the format-3 automatic-ID allocator issues no second ID, and there is no `orna reset` to move a branch back",
            second_automatic_id_is_refused,
        ),
        Entry::pass("TXN-001", nested_writes_roll_back_with_the_activation),
        Entry::gap(
            "TXN-003",
            "ornadb-5il1.89",
            "project source cannot perform an HTTP effect: `http.get` does not resolve",
            http_effect_does_not_resolve,
        ),
        Entry::gap(
            "CP-001",
            "ornadb-5il1.134",
            "stream checkpoints are not observable: `sys.Checkpoint` does not evaluate in the project REPL",
            checkpoints_are_unobservable,
        ),
        Entry::gap(
            "FAIL-001",
            "ornadb-5il1.134",
            "failed deliveries are neither observable nor administrable: `sys.Failure` does not evaluate in the project REPL",
            failures_are_unobservable,
        ),
        Entry::gap(
            "FAIL-002",
            "ornadb-5il1.134",
            SKIP_BLOCKER,
            failures_are_unobservable,
        ),
        Entry::gap(
            "FAIL-003",
            "ornadb-5il1.134",
            SKIP_BLOCKER,
            failures_are_unobservable,
        ),
        Entry::pass("PUB-001", crash_before_the_ref_update_preserves_the_batch),
        Entry::pass("PUB-002", crash_after_the_ref_update_completes_once),
        Entry::pass("PUB-004", publication_commits_only_managed_paths),
        Entry::pass("PUB-006", publication_keeps_a_partially_staged_edit),
        Entry::gap(
            "ASSERT-MERGE-091",
            "ornadb-mmsa.29.1",
            "there is no `orna merge`, so no semantic merge validates a combined candidate",
            |db| unknown_command(db, &["merge", "feature"]),
        ),
        Entry::gap(
            "SYS-001",
            "ornadb-gov5.12.10",
            "`sys.Run` does not evaluate in the project REPL, so no historical run query exists",
            |db| repl_refuses(db, &["sys.Run.as_of(HEAD)"], "ORNA-EVAL-NAME"),
        ),
        Entry::gap(
            "LIVE-002",
            "ornadb-5il1.144",
            LIVE_BLOCKER,
            watches_receive_no_update,
        ),
        Entry::gap(
            "LIVE-004",
            "ornadb-5il1.144",
            LIVE_BLOCKER,
            watches_receive_no_update,
        ),
        Entry::gap(
            "PRES-001",
            "ornadb-5il1.142",
            "the REPL has no `:display` override command",
            |db| {
                repl_refuses(
                    db,
                    &[":display Duration std.time.duration.clock"],
                    "ORNA-REPL-COMMAND",
                )
            },
        ),
        Entry::gap(
            "GIT-001",
            "ornadb-gov5.21",
            "there is no `orna clone`, so no partial clone hydrates only the segments a query needs",
            |db| unknown_command(db, &["clone", "--filter=blob:none", ".", "copy"]),
        ),
    ]
}

const CONTACT_SOURCE: &str = "pub table Contact(id: Str) { name: Str, }\n";
const INSERT_ALICE: &str = r#"main.Contact.insert({ id: "alice", name: "Alice" })"#;

fn repl_cannot_read_a_table(db: &mut Db) -> Probe {
    db.declare(CONTACT_SOURCE)?;
    repl_refuses(db, &["main.Contact | count()"], "ORNA-EVAL-NAME")
}

fn second_automatic_id_is_refused(db: &mut Db) -> Probe {
    db.declare("pub table Note { text: Str, }\n")?;
    let first = db.repl_effect(r#"main.Note.insert({ text: "first" })"#)?;
    ensure!(
        first.stdout.contains("\"id\": 1"),
        "the first automatic ID is not 1: {}",
        first.output()
    );
    let second = db.repl(&[r#"main.Note.insert({ text: "second" })"#])?;
    ensure!(
        second.output().contains("error["),
        "a second automatic-ID insert is now accepted: {}",
        second.output()
    );
    Ok(format!(
        "second automatic-ID insert: {}",
        second.diagnostic()
    ))
}

const TRANSACTION_SOURCE: &str = r#"pub table Note(id: Str) { text: Str, }

fn child() {
    Note.insert({ id: "child", text: "written by the child" });
}

pub fn parent_fails() {
    child();
    fail(error(code: "scenario.parent", message: "the parent fails after its child wrote"));
}

pub fn parent_succeeds() {
    child();
}
"#;

/// TXN-001: the child's insert is part of the parent's activation, so the
/// parent's failure removes it; the same child write survives when the
/// parent succeeds.
fn nested_writes_roll_back_with_the_activation(db: &mut Db) -> Check {
    db.declare(TRANSACTION_SOURCE)?;
    ensure!(
        db.runtime_mutations()? == 0,
        "a new database has no mutations"
    );
    let failed = db.orna(&["run", "main.parent_fails"])?;
    ensure!(
        !failed.succeeded() && failed.output().contains("failed atomically"),
        "the failing parent did not report an atomic rollback: {}",
        failed.output()
    );
    ensure!(
        db.runtime_mutations()? == 0,
        "the child's insert survived its parent's failure"
    );
    db.orna(&["run", "main.parent_succeeds"])?.ok()?;
    ensure!(
        db.runtime_mutations()? == 1,
        "the child's insert is not recorded when its parent succeeds"
    );
    Ok(())
}

fn http_effect_does_not_resolve(db: &mut Db) -> Probe {
    check_refuses(
        db,
        r#"pub table Note(id: Str) { text: Str, }

pub fn notify() {
    Note.insert({ id: "sent", text: "before the effect" });
    http.get("http://127.0.0.1:9/scenario");
    fail(error(code: "scenario.after", message: "the activation fails after its effect"));
}
"#,
        "ORNA-S012-UNRESOLVED",
    )
}

fn checkpoints_are_unobservable(db: &mut Db) -> Probe {
    db.declare(
        r#"pub table Email(id: Int) { subject: Str, }

pub fn source() = Stream.from_list([41, 42], source_identity: "scenario:mail:v1");

pub fn ingest() {
    source() | for_each(item => {
        Email.insert({ id: item, subject: "delivered" });
    });
}
"#,
    )?;
    db.orna(&["run", "main.ingest"])?.ok()?;
    repl_refuses(db, &["sys.Checkpoint"], "ORNA-EVAL-NAME")
}

fn failures_are_unobservable(db: &mut Db) -> Probe {
    db.declare(CONTACT_SOURCE)?;
    repl_refuses(db, &["sys.Failure"], "ORNA-EVAL-NAME")
}

/// PUB-004: the publication commit holds only Orna-managed data paths, and
/// a staged new module, an unstaged edit and an untracked file stay exactly
/// as the user left them (ORNA-PUB-010, ORNA-PUB-011).
fn publication_commits_only_managed_paths(db: &mut Db) -> Check {
    let tables = db.declare(CONTACT_SOURCE)?;
    let relation = Db::relation(&tables, "Contact")?;
    db.repl_effect(INSERT_ALICE)?;
    db.write("helper.orna", "pub fn helper(): Int = 1;\n")?;
    db.git(&["add", "helper.orna"])?.ok()?;
    db.write(
        "main.orna",
        format!("{CONTACT_SOURCE}// an unstaged human edit\n"),
    )?;
    db.write("notes.txt", "untracked human notes\n")?;
    published_only_managed_paths(db, &relation)?;
    ensure!(
        db.read("notes.txt")? == b"untracked human notes\n",
        "the untracked file changed"
    );
    Ok(())
}

/// PUB-006: a file whose staged and worktree versions differ keeps both
/// versions across publication (ORNA-PUB-007..009).
fn publication_keeps_a_partially_staged_edit(db: &mut Db) -> Check {
    let tables = db.declare(CONTACT_SOURCE)?;
    let relation = Db::relation(&tables, "Contact")?;
    db.repl_effect(INSERT_ALICE)?;
    db.write(
        "main.orna",
        format!("{CONTACT_SOURCE}pub fn staged(): Int = 1;\n"),
    )?;
    db.git(&["add", "main.orna"])?.ok()?;
    db.write(
        "main.orna",
        format!("{CONTACT_SOURCE}pub fn staged(): Int = 1;\npub fn unstaged(): Int = 2;\n"),
    )?;
    published_only_managed_paths(db, &relation)
}

/// Publishes the pending rows and checks that the machine commit changes
/// only `.orna/store` on top of the old `HEAD`, while the human staged and
/// unstaged differences are byte-identical.
fn published_only_managed_paths(db: &mut Db, relation: &str) -> Check {
    let staged = db.git_text(&["diff", "--cached", "--binary"])?;
    let unstaged = db.git_text(&["diff", "--binary"])?;
    ensure!(
        !staged.is_empty() && !unstaged.is_empty(),
        "the scenario has staged and unstaged human work"
    );
    let base = db.git_text(&["rev-parse", "HEAD"])?.trim().to_owned();
    db.publish()?.ok()?;
    let head = db.git_text(&["rev-parse", "HEAD"])?.trim().to_owned();
    ensure!(head != base, "publication did not advance the branch");
    let parent = db.git_text(&["rev-parse", "HEAD^"])?.trim().to_owned();
    ensure!(
        parent == base,
        "the publication commit is not a child of the old HEAD"
    );
    let changed = db.git_text(&[
        "diff-tree",
        "--no-commit-id",
        "--name-only",
        "-r",
        &base,
        &head,
    ])?;
    ensure!(
        !changed.is_empty() && changed.lines().all(|path| path.starts_with(".orna/store/")),
        "the publication commit changed paths outside the managed store: {changed}"
    );
    ensure!(
        db.git_text(&["diff", "--cached", "--binary"])? == staged,
        "the human index changed"
    );
    ensure!(
        db.git_text(&["diff", "--binary"])? == unstaged,
        "the human worktree changed"
    );
    ensure!(
        db.query_json(relation, &[])?["rows"] == 1,
        "the published row is not in the committed relation"
    );
    ensure!(
        db.runtime_mutations()? == 0,
        "the published batch is still pending"
    );
    Ok(())
}

const CRASH_AT: &str = "ORNA_TEST_PUBLICATION_CRASH_AT";
const CRASH_DIR: &str = "ORNA_TEST_PUBLICATION_CRASH_DIR";
const CRASH_MARKER: &str = "publication-crash-marker";

/// Starts `orna publish` armed to park at `boundary`, waits until it
/// reaches it, then kills it.
fn kill_publication_at(db: &mut Db, boundary: &str) -> Check {
    let crash = db.scratch(&format!("crash-{boundary}"))?;
    let crash_text = crash
        .to_str()
        .ok_or_else(|| Failure("non-UTF-8 crash directory".to_owned()))?
        .to_owned();
    let mut child = db.spawn_orna(
        &["publish", "--message", "Publish the scenario rows"],
        &[(CRASH_AT, boundary), (CRASH_DIR, &crash_text)],
    )?;
    let reached = wait_for_marker(&mut child, &crash.join(CRASH_MARKER));
    let _ = child.kill();
    let _ = child.wait();
    reached
}

fn wait_for_marker(child: &mut Child, marker: &Path) -> Check {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if marker.exists() {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(Failure(format!(
                "the publisher exited ({status}) before its crash point"
            )));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(Failure(
        "the publisher never reached its crash point".to_owned(),
    ))
}

/// PUB-001: a publisher killed after writing its candidate objects and
/// before the ref update leaves the branch and the pending batch unchanged;
/// the next publication publishes the batch once (ORNA-PUB-005/006).
fn crash_before_the_ref_update_preserves_the_batch(db: &mut Db) -> Check {
    let tables = db.declare(CONTACT_SOURCE)?;
    let relation = Db::relation(&tables, "Contact")?;
    db.repl_effect(INSERT_ALICE)?;
    let base = db.git_text(&["rev-parse", "HEAD"])?.trim().to_owned();
    kill_publication_at(db, "candidate")?;
    ensure!(
        db.git_text(&["rev-parse", "HEAD"])?.trim() == base,
        "the branch moved although the publisher died before its ref update"
    );
    ensure!(
        db.runtime_mutations()? == 1,
        "the pending batch did not survive the crash"
    );
    db.publish()?.ok()?;
    ensure!(
        db.git_text(&["rev-parse", "HEAD^"])?.trim() == base,
        "the recovered publication is not one commit on the old HEAD"
    );
    ensure!(
        db.query_json(&relation, &[])?["rows"] == 1,
        "the recovered publication does not hold the row exactly once"
    );
    ensure!(
        db.runtime_mutations()? == 0,
        "the published batch is still pending"
    );
    Ok(())
}

/// PUB-002: a publisher killed after its ref update has published the batch;
/// the next start completes the cleanup without a second commit or a
/// duplicate row (ORNA-PUB-007/008).
fn crash_after_the_ref_update_completes_once(db: &mut Db) -> Check {
    let tables = db.declare(CONTACT_SOURCE)?;
    let relation = Db::relation(&tables, "Contact")?;
    db.repl_effect(INSERT_ALICE)?;
    let base = db.git_text(&["rev-parse", "HEAD"])?.trim().to_owned();
    kill_publication_at(db, "ref-cas")?;
    let published = db.git_text(&["rev-parse", "HEAD"])?.trim().to_owned();
    ensure!(
        published != base,
        "the ref update did not land before the crash"
    );
    ensure!(
        db.git_text(&["rev-parse", "HEAD^"])?.trim() == base,
        "the published batch is not one commit on the old HEAD"
    );
    ensure!(
        db.query_json(&relation, &[])?["rows"] == 1,
        "the committed batch does not hold the row"
    );
    db.publish()?.ok()?;
    ensure!(
        db.git_text(&["rev-parse", "HEAD"])?.trim() == published,
        "recovery published the batch a second time"
    );
    ensure!(
        db.runtime_mutations()? == 0,
        "recovery left the batch pending"
    );
    ensure!(
        db.query_json(&relation, &[])?["rows"] == 1,
        "recovery duplicated the row"
    );
    let again = db.publish()?.ok()?;
    ensure!(
        db.git_text(&["rev-parse", "HEAD"])?.trim() == published,
        "a later publication repeated the batch: {}",
        again.output()
    );
    Ok(())
}

/// LIVE-002/004: a watch over a value derived from a table receives its
/// first snapshot, then nothing after the table changes. The probe fails
/// (the gap is stale) once the host pushes a snapshot or delta for the
/// change, or if the watch request itself is malformed.
pub fn watches_receive_no_update(db: &mut Db) -> Probe {
    db.declare(
        "pub table Note(id: Str) { text: Str, }\n\npub fn notes(): Int = Note | count();\n",
    )?;
    let database = db.database_id()?;
    let server = db.serve()?;
    let origin = format!("http://127.0.0.1:{}", server.port());
    let session = server.request(
        "POST",
        "/orna/session",
        &[("Content-Type", "application/json"), ("Origin", &origin)],
        format!(r#"{{"database":"{database}","protocol":"orna.present.v1"}}"#).as_bytes(),
    )?;
    ensure!(
        session.status == 201,
        "the live session was not created: {} {}",
        session.status,
        session.body_text()
    );
    let body: serde_json::Value = serde_json::from_slice(&session.body)
        .map_err(|error| Failure(format!("session response is not JSON: {error}")))?;
    let path = body["websocket_path"]
        .as_str()
        .ok_or_else(|| Failure(format!("no websocket_path in {body}")))?;
    let cookie = session
        .head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .starts_with("set-cookie:")
                .then(|| line["set-cookie:".len()..].trim().to_owned())
        })
        .and_then(|value| value.split(';').next().map(str::to_owned))
        .ok_or_else(|| Failure("the session response sets no cookie".to_owned()))?;

    let header = |value: &str| {
        HeaderValue::from_str(value).map_err(|error| Failure(format!("header {value}: {error}")))
    };
    let mut request = format!("ws://127.0.0.1:{}{path}", server.port())
        .into_client_request()
        .map_err(|error| Failure(format!("websocket request: {error}")))?;
    request.headers_mut().insert("Origin", header(&origin)?);
    request.headers_mut().insert("Cookie", header(&cookie)?);
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", header("orna.present.v1")?);
    let stream = TcpStream::connect(("127.0.0.1", server.port()))
        .map_err(|error| Failure(format!("connect: {error}")))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .map_err(|error| Failure(format!("socket timeout: {error}")))?;
    let (mut socket, _) = tungstenite::client(request, stream)
        .map_err(|error| Failure(format!("websocket upgrade: {error}")))?;
    socket
        .send(Message::Binary(
            watch_envelope(&database, "main.notes()")?.into(),
        ))
        .map_err(|error| Failure(format!("send watch: {error}")))?;
    let first = read_envelope(&mut socket)?;
    match message_type(&first) {
        Some(SNAPSHOT) => {}
        Some(DIAGNOSTIC) => {
            // A `wire.*` code means this request is malformed, which says
            // nothing about the product.
            let text = printable(&first);
            ensure!(
                !text.contains("wire."),
                "the watch request is malformed: {text}"
            );
            return Ok(format!("watch on `main.notes()` refused: {text}"));
        }
        other => {
            return Err(Failure(format!(
                "the watch got message type {other:?} instead of a snapshot: {}",
                printable(&first)
            )));
        }
    }

    // A second process changes the watched relation's CWD.
    db.repl_effect(r#"main.Note.insert({ id: "later", text: "changes the watched count" })"#)?;
    socket
        .get_mut()
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|error| Failure(format!("socket timeout: {error}")))?;
    match socket.read() {
        Ok(Message::Binary(bytes)) => Err(Failure(format!(
            "the host now pushes message type {:?} after the change",
            message_type(&bytes)
        ))),
        Ok(other) => Err(Failure(format!("unexpected live message {other:?}"))),
        Err(tungstenite::Error::Io(error))
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Ok("watch on `main.notes()`: snapshot 0 received; no snapshot or delta within 10 s of a CWD insert".to_owned())
        }
        Err(error) => Err(Failure(format!("live socket failed: {error}"))),
    }
}

const SNAPSHOT: u8 = 16;
const DIAGNOSTIC: u8 = 19;

/// The printable ASCII runs of a binary message, for evidence lines.
fn printable(bytes: &[u8]) -> String {
    bytes
        .split(|byte| !(0x20..0x7f).contains(byte))
        .filter(|run| run.len() >= 3)
        .map(|run| String::from_utf8_lossy(run).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A canonical OVB-1 `watch` envelope (30-protocol registry type 5) with a
/// CWD database context and a plain terminal presentation context.
fn watch_envelope(database: &str, source: &str) -> Result<Vec<u8>, Failure> {
    let database = uuid_bytes(database)?;
    let mut envelope = vec![0xa5, 0x00, 0x01, 0x01, 0x05, 0x02, 0x50];
    envelope.extend_from_slice(&[0x5c; 16]);
    envelope.extend_from_slice(&[0x03, 0xf6, 0x04, 0xa3, 0x00]);
    cbor_text(&mut envelope, source);
    envelope.extend_from_slice(&[0x01, 0xa2, 0x00, 0xd8, 0x25, 0x50]);
    envelope.extend_from_slice(&database);
    envelope.extend_from_slice(&[0x01, 0xf6, 0x02, 0xa5, 0x00]);
    cbor_text(&mut envelope, "en-GB");
    envelope.extend_from_slice(&[0x01, 0xf6, 0x02, 0xf6, 0x03]);
    cbor_text(&mut envelope, "terminal/plain");
    envelope.extend_from_slice(&[0x04, 0x80]);
    Ok(envelope)
}

fn cbor_text(output: &mut Vec<u8>, text: &str) {
    let length = text.len();
    if length < 24 {
        output.push(0x60 | length as u8);
    } else {
        output.push(0x78);
        output.push(u8::try_from(length).expect("short CBOR text"));
    }
    output.extend_from_slice(text.as_bytes());
}

fn uuid_bytes(text: &str) -> Result<[u8; 16], Failure> {
    let digits = text.replace('-', "");
    let mut bytes = [0u8; 16];
    ensure!(digits.len() == 32, "{text} is not a UUID");
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[index * 2..index * 2 + 2], 16)
            .map_err(|_| Failure(format!("{text} is not a UUID")))?;
    }
    Ok(bytes)
}

fn read_envelope(socket: &mut tungstenite::WebSocket<TcpStream>) -> Result<Vec<u8>, Failure> {
    loop {
        match socket
            .read()
            .map_err(|error| Failure(format!("read live message: {error}")))?
        {
            Message::Binary(bytes) => return Ok(bytes.to_vec()),
            Message::Ping(_) | Message::Pong(_) => {}
            other => return Err(Failure(format!("unexpected live message {other:?}"))),
        }
    }
}

/// The registry type of a canonical envelope: a five-entry map whose first
/// two entries are `0: 1` and `1: <type>`.
fn message_type(envelope: &[u8]) -> Option<u8> {
    match envelope {
        [0xa5, 0x00, 0x01, 0x01, kind, ..] if *kind < 24 => Some(*kind),
        [0xa5, 0x00, 0x01, 0x01, 0x18, kind, ..] => Some(*kind),
        _ => None,
    }
}
