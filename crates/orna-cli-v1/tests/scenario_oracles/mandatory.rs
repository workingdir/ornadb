//! Entries for the retained oracles behind the §39 mandatory behavioural
//! scenarios.

use std::{
    io::{Read, Write},
    net::TcpStream,
    path::Path,
    process::Child,
    thread,
    time::{Duration, Instant},
};

use crate::harness::{
    Check, Db, Entry, Failure, Probe, check_refuses, ensure, harness_ensure, one_line,
    repl_refuses, unknown_command,
};

pub const LIVE_BLOCKER: &str = "the live watch socket is closed with code 1002 before the first snapshot for the spec-shaped `main.notes()` watch request, so ORNA-WIRE-001's initial snapshot is not delivered; root cause unverified (host rejection or probe envelope)";
const SKIP_BLOCKER: &str = "an operator cannot skip a failed delivery: `sys.Failure` does not evaluate in the project REPL";

pub fn entries() -> Vec<Entry> {
    vec![
        Entry::gap(
            "STATE-001",
            "ornadb-5afo",
            "the CWD relation does not count uncommitted REPL inserts: `main.Contact | count()` ignores the local row (ORNA-STATE-002)",
            cwd_overlays_head,
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
            "`orna clone --filter` is ignored for local clones: the clone copies every object, so no partial clone hydrates only the segments a query needs",
            clone_filter_is_ignored,
        ),
    ]
}

const CONTACT_SOURCE: &str = "pub table Contact(id: Str) { name: Str, }\n";
const INSERT_ALICE: &str = r#"main.Contact.insert({ id: "alice", name: "Alice" })"#;

fn cwd_overlays_head(db: &mut Db) -> Probe {
    db.declare(CONTACT_SOURCE)?;
    db.repl_effect(INSERT_ALICE)?;
    db.publish()?.ok()?;
    db.repl_effect(r#"main.Contact.insert({ id: "bob", name: "Bob" })"#)?;
    let cwd = db.repl(&["main.Contact | count()"])?;
    let head = db.repl(&["main.Contact.as_of(HEAD) | count()"])?;
    let evidence = format!(
        "`main.Contact | count()` after an uncommitted insert: {}; `main.Contact.as_of(HEAD) | count()`: {}",
        cwd.output().trim(),
        head.output().trim()
    );
    if cwd.output().contains("> 2 : Int") && head.output().contains("> 1 : Int") {
        return Err(Failure::Unmet(format!(
            "the CWD relation now counts the local row and HEAD counts only the published row: {evidence}"
        )));
    }
    harness_ensure!(
        cwd.output().contains("> 1 : Int"),
        "the CWD count is not in the expected form, so the blocker was not observed: {evidence}"
    );
    Ok(evidence)
}

fn clone_filter_is_ignored(db: &mut Db) -> Probe {
    let run = db.orna(&["clone", "--filter=blob:none", ".", "copy"])?;
    ensure!(
        run.succeeded() && run.output().contains("--filter is ignored in local clones"),
        "`orna clone --filter=blob:none` now honours its filter in a local clone: {}",
        run.output()
    );
    Ok(format!(
        "orna clone --filter=blob:none . copy: {}",
        one_line(&run.output(), "--filter is ignored")
    ))
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
    harness_ensure!(
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
        .ok_or_else(|| Failure::Harness("non-UTF-8 crash directory".to_owned()))?
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
            return Err(Failure::Harness(format!(
                "the publisher exited ({status}) before its crash point"
            )));
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(Failure::Harness(
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
/// first snapshot, then nothing after the table changes. The probe is stale
/// once the host pushes any message for the change. A malformed watch request
/// is a harness error, not evidence about the product.
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
    harness_ensure!(
        session.status == 201,
        "the live session was not created: {} {}",
        session.status,
        session.body_text()
    );
    let body: serde_json::Value = serde_json::from_slice(&session.body)
        .map_err(|error| Failure::Harness(format!("session response is not JSON: {error}")))?;
    let path = body["websocket_path"]
        .as_str()
        .ok_or_else(|| Failure::Harness(format!("no websocket_path in {body}")))?;
    let cookie = session
        .head
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .starts_with("set-cookie:")
                .then(|| line["set-cookie:".len()..].trim().to_owned())
        })
        .and_then(|value| value.split(';').next().map(str::to_owned))
        .ok_or_else(|| Failure::Harness("the session response sets no cookie".to_owned()))?;

    let mut socket = WatchSocket::connect(
        server.port(),
        path,
        &[
            ("Origin", origin.as_str()),
            ("Cookie", cookie.as_str()),
            ("Sec-WebSocket-Protocol", "orna.present.v1"),
        ],
    )?;
    socket.send_binary(&watch_envelope(&database, "main.notes()")?)?;
    let first = match socket.read_message(Duration::from_secs(60))? {
        Received::Message(bytes) => bytes,
        Received::Idle => {
            return Err(Failure::Harness(
                "no first live message within 60 s".to_owned(),
            ));
        }
        Received::Closed(1002) => {
            return Ok(
                "watch on `main.notes()`: the host closed the live socket with close code 1002 before the first snapshot"
                    .to_owned(),
            );
        }
        Received::Closed(code) => {
            return Err(Failure::Unmet(format!(
                "the host closed the live socket with close code {code} before the first snapshot, not 1002"
            )));
        }
    };
    match message_type(&first) {
        Some(SNAPSHOT) => {}
        Some(DIAGNOSTIC) => {
            // A `wire.*` code means this request is malformed, which says
            // nothing about the product.
            let text = printable(&first);
            harness_ensure!(
                !text.contains("wire."),
                "the watch request is malformed: {text}"
            );
            return Ok(format!("watch on `main.notes()` refused: {text}"));
        }
        other => {
            return Err(Failure::Harness(format!(
                "the watch got message type {other:?} instead of a snapshot: {}",
                printable(&first)
            )));
        }
    }

    // A second process changes the watched relation's CWD.
    db.repl_effect(r#"main.Note.insert({ id: "later", text: "changes the watched count" })"#)?;
    match socket.read_message(Duration::from_secs(10))? {
        Received::Idle => Ok(
            "watch on `main.notes()`: snapshot 0 received; no snapshot or delta within 10 s of a CWD insert"
                .to_owned(),
        ),
        Received::Closed(1002) => Ok(
            "watch on `main.notes()`: snapshot 0 received; after a CWD insert, the host closed the live socket with close code 1002"
                .to_owned(),
        ),
        Received::Closed(code) => Err(Failure::Unmet(format!(
            "after a CWD insert the host closed the live socket with close code {code}, not 1002"
        ))),
        Received::Message(bytes) => Err(Failure::Unmet(format!(
            "the host now pushes message type {:?} after the change",
            message_type(&bytes)
        ))),
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
            .map_err(|_| Failure::Harness(format!("{text} is not a UUID")))?;
    }
    Ok(bytes)
}

/// The `Sec-WebSocket-Key` the probe sends. Any 16-byte base64 value is valid;
/// the server's accept key is not checked because the probe only reads frames.
const WATCH_KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";

/// What one read from the live socket produced.
enum Received {
    /// A binary message.
    Message(Vec<u8>),
    /// Nothing arrived before the timeout.
    Idle,
    /// The host sent a close frame with this status code.
    Closed(u16),
}

/// A minimal RFC 6455 client for the live watch probe: one HTTP upgrade, masked
/// client frames and unmasked server frames. It adds no dependency, so the
/// workspace lockfile is unchanged.
struct WatchSocket {
    stream: TcpStream,
}

impl WatchSocket {
    fn connect(port: u16, path: &str, headers: &[(&str, &str)]) -> Result<Self, Failure> {
        let mut stream = TcpStream::connect(("127.0.0.1", port))
            .map_err(|error| Failure::Harness(format!("connect: {error}")))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .map_err(|error| Failure::Harness(format!("socket timeout: {error}")))?;
        let extra: String = headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\r\n"))
            .collect();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {WATCH_KEY}\r\n{extra}\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|error| Failure::Harness(format!("websocket upgrade: {error}")))?;
        let head = read_head(&mut stream)?;
        harness_ensure!(
            head.starts_with("HTTP/1.1 101"),
            "websocket upgrade refused: {}",
            head.lines().next().unwrap_or_default()
        );
        Ok(Self { stream })
    }

    /// Sends one binary frame, masked as RFC 6455 requires of clients.
    fn send_binary(&mut self, payload: &[u8]) -> Result<(), Failure> {
        self.send_frame(0x2, payload)
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), Failure> {
        const MASK: [u8; 4] = [0x37, 0xfa, 0x21, 0x3d];
        let mut frame = vec![0x80 | opcode];
        match payload.len() {
            length @ 0..=125 => frame.push(0x80 | length as u8),
            length @ 126..=0xffff => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(length as u16).to_be_bytes());
            }
            length => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(length as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(&MASK);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ MASK[index % 4]),
        );
        self.stream
            .write_all(&frame)
            .map_err(|error| Failure::Harness(format!("send live frame: {error}")))
    }

    /// Waits up to `timeout` for the next message. Pings are answered, a close
    /// frame is returned with its status code, and any other frame is an error.
    fn read_message(&mut self, timeout: Duration) -> Result<Received, Failure> {
        self.stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| Failure::Harness(format!("socket timeout: {error}")))?;
        loop {
            let mut header = [0u8; 2];
            match self.stream.read_exact(&mut header) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(Received::Idle);
                }
                Err(error) => {
                    return Err(Failure::Harness(format!("live socket failed: {error}")));
                }
            }
            harness_ensure!(header[1] & 0x80 == 0, "the host sent a masked frame");
            let length = match header[1] & 0x7f {
                126 => {
                    let mut bytes = [0u8; 2];
                    self.read_frame_bytes(&mut bytes)?;
                    usize::from(u16::from_be_bytes(bytes))
                }
                127 => {
                    let mut bytes = [0u8; 8];
                    self.read_frame_bytes(&mut bytes)?;
                    usize::try_from(u64::from_be_bytes(bytes))
                        .map_err(|_| Failure::Harness("live frame length overflows".to_owned()))?
                }
                short => usize::from(short),
            };
            let mut payload = vec![0u8; length];
            self.read_frame_bytes(&mut payload)?;
            match header[0] & 0x0f {
                0x2 => return Ok(Received::Message(payload)),
                0x9 => self.send_frame(0xa, &payload)?,
                0xa => {}
                0x8 => {
                    let code = payload
                        .get(..2)
                        .map_or(0, |bytes| u16::from_be_bytes([bytes[0], bytes[1]]));
                    return Ok(Received::Closed(code));
                }
                other => {
                    return Err(Failure::Harness(format!(
                        "unexpected live frame opcode {other:#x}"
                    )));
                }
            }
        }
    }

    fn read_frame_bytes(&mut self, bytes: &mut [u8]) -> Result<(), Failure> {
        self.stream
            .read_exact(bytes)
            .map_err(|error| Failure::Harness(format!("read live frame: {error}")))
    }
}

/// Reads an HTTP response head byte by byte, so no frame that follows the
/// upgrade is consumed with it.
fn read_head(stream: &mut TcpStream) -> Result<String, Failure> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
    harness_ensure!(
            head.len() < 16 * 1024,
            "the websocket upgrade response has no end"
        );
        stream
            .read_exact(&mut byte)
            .map_err(|error| Failure::Harness(format!("websocket upgrade: {error}")))?;
        head.push(byte[0]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
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
