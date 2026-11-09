use super::*;
use std::process::Command;

#[test]
fn websocket_failures_close_with_size_or_protocol_code() {
    assert_eq!(
        close_code_for_failure(orna_live::Error::Limit),
        1009,
        "an oversized message closes with 1009"
    );
    assert_eq!(
        close_code_for_failure(orna_live::Error::InvalidFrame),
        1002,
        "a malformed frame closes with 1002"
    );
    assert_eq!(
        close_code_for_failure(orna_live::Error::InvalidMessage),
        1002,
        "a malformed message closes with 1002"
    );
}

#[test]
fn accept_retries_transient_failures_and_stops_on_fatal_ones() {
    assert!(is_transient_accept_error(&io::Error::from(
        io::ErrorKind::ConnectionAborted
    )));
    assert!(is_transient_accept_error(&io::Error::from(
        io::ErrorKind::Interrupted
    )));
    assert!(
        is_transient_accept_error(&io::Error::from_raw_os_error(24)),
        "EMFILE retries after descriptors are released"
    );
    assert!(is_transient_accept_error(&io::Error::from_raw_os_error(23)));
    assert!(!is_transient_accept_error(&io::Error::from(
        io::ErrorKind::PermissionDenied
    )));
    assert!(!is_transient_accept_error(&io::Error::from_raw_os_error(
        22
    )));
}

/// Writes `request` from a client socket and returns the server side of
/// the connection, buffered the way `serve_connection` reads it.
fn buffered_request(request: &[u8]) -> BufReader<TcpStream> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    client.write_all(request).unwrap();
    let (server, _) = listener.accept().unwrap();
    BufReader::new(server)
}

fn keepalive(ping_after: u64, idle_timeout: u64) -> WebSocketKeepalive {
    WebSocketKeepalive {
        ping_after: StdDuration::from_secs(ping_after),
        idle_timeout: StdDuration::from_secs(idle_timeout),
        write_timeout: StdDuration::from_secs(30),
    }
}

#[test]
fn silence_pings_once_and_closes_at_the_idle_bound() {
    let start = Instant::now();
    let at = |seconds: u64| start + StdDuration::from_secs(seconds);
    let mut silence = Silence::new(keepalive(15, 30));
    silence.heard(start);
    assert_eq!(
        silence.next_step(at(0)),
        SilenceStep::Wait(StdDuration::from_secs(15))
    );
    assert_eq!(
        silence.next_step(at(10)),
        SilenceStep::Wait(StdDuration::from_secs(5))
    );
    // Past the ping bound one Ping is owed, then the wait runs to the idle bound.
    assert_eq!(
        silence.next_step(at(15)),
        SilenceStep::Ping {
            wait: StdDuration::from_secs(15)
        }
    );
    assert_eq!(
        silence.next_step(at(20)),
        SilenceStep::Wait(StdDuration::from_secs(10))
    );
    assert_eq!(silence.next_step(at(30)), SilenceStep::Close);
}

#[test]
fn bytes_from_the_peer_restart_silence_and_owe_a_new_ping() {
    let start = Instant::now();
    let at = |seconds: u64| start + StdDuration::from_secs(seconds);
    let mut silence = Silence::new(keepalive(15, 30));
    silence.heard(start);
    assert!(matches!(
        silence.next_step(at(16)),
        SilenceStep::Ping { .. }
    ));
    silence.heard(at(20));
    assert_eq!(
        silence.next_step(at(21)),
        SilenceStep::Wait(StdDuration::from_secs(14))
    );
    assert_eq!(
        silence.next_step(at(35)),
        SilenceStep::Ping {
            wait: StdDuration::from_secs(15)
        }
    );
}

#[test]
fn malformed_orna_head_is_answered_in_the_live_error_shape() {
    let mut reader = buffered_request(b"GET /orna/session HTTP/1.1\r\nno-colon\r\n\r\n");
    let mut orna_route = false;
    let deadline = Instant::now() + StdDuration::from_secs(5);
    assert!(read_request_head(&mut reader, deadline, &mut orna_route).is_err());
    assert!(orna_route, "the request line already chose the live route");
    assert_eq!(
        live_error_response(400, "wire.invalid_message", "malformed request").body,
        br#"{"code":"wire.invalid_message","message":"malformed request"}"#.to_vec()
    );
}

#[test]
fn html_pages_carry_csp_nosniff_framing_and_referrer_headers() {
    let html = String::from_utf8(response_head(&Response::new(
        200,
        "text/html; charset=utf-8",
        Vec::new(),
    )))
    .expect("HTML response head");
    assert!(
        html.contains("Content-Type: text/html; charset=utf-8"),
        "{html}"
    );
    for header in [
        "Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        "X-Content-Type-Options: nosniff",
        "X-Frame-Options: DENY",
        "Referrer-Policy: no-referrer",
    ] {
        assert!(html.contains(header), "lacks {header}: {html}");
    }
    let plain = String::from_utf8(response_head(&Response::new(
        200,
        "text/plain; charset=utf-8",
        Vec::new(),
    )))
    .expect("plain response head");
    assert!(!plain.contains("Content-Security-Policy"), "{plain}");
}

#[test]
fn oversized_non_orna_body_is_413_and_foreign_origin_is_refused_before_sizing() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=sizes"],
    );
    let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
    let host = address.to_string();
    let declared = MAX_REQUEST_BODY_BYTES + 1;

    let own = exchange(
        address,
        &format!(
            "POST /api/x/ HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\nContent-Length: {declared}\r\n\r\n"
        ),
    );
    assert_eq!(own.status, 413, "{}", own.head);
    assert_eq!(own.body, r#"{"error":"payload_too_large"}"#);

    let foreign = exchange(
        address,
        &format!(
            "POST /api/x/ HTTP/1.1\r\nHost: {host}\r\nOrigin: http://evil.example\r\nContent-Length: {declared}\r\n\r\n"
        ),
    );
    assert_eq!(
        foreign.status, 403,
        "a foreign origin is refused before its body size is judged: {}",
        foreign.head
    );
}

#[test]
fn oversized_orna_body_is_refused_by_its_declared_length() {
    let declared = MAX_REQUEST_BODY_BYTES + 1;
    let head =
        format!("POST /orna/session HTTP/1.1\r\nHost: x\r\nContent-Length: {declared}\r\n\r\n");
    let mut reader = buffered_request(head.as_bytes());
    let mut orna_route = false;
    let deadline = Instant::now() + StdDuration::from_secs(5);
    let (request, body_length) = read_request_head(&mut reader, deadline, &mut orna_route)
        .expect("a well-formed head parses");
    assert!(orna_route);
    assert_eq!(body_length, declared);
    assert!(body_length > request_body_limit(&request.path));
    assert_eq!(
        live_error_response(413, "wire.limit", "request body exceeds its limit").status,
        413
    );
}

const SERVE_FIXTURE: &str = include_str!("../../../tests/fixtures/serve-no-autoload.orna");

fn git_succeeds(directory: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .output()
        .expect("run Git");
    assert!(
        output.status.success(),
        "Git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn content_page(root: &Path, path: &str) -> Response {
    host_route(
        root,
        RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        },
        false,
        &Request {
            method: "GET".into(),
            path: path.into(),
            query: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
        },
    )
}

/// ORNA-SERVE-002: `orna serve` MUST NOT automatically invoke root `main()` or unrelated stream programs.
#[test]
fn serving_loads_the_local_main_module_without_evaluating_it() {
    let directory = tempfile::tempdir().expect("temporary clone");
    std::fs::write(directory.path().join("main.orna"), SERVE_FIXTURE)
        .expect("vendored main module");
    assert!(
        Command::new("git")
            .args(["init", "--quiet", "--initial-branch", "serve-test"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );
    let repository = Repository::discover(directory.path()).expect("clone");
    let loaded = orna_project::ProjectLoader::default()
        .load(&repository)
        .expect("loading parses but does not run main");
    assert_eq!(loaded.identities()[0].logical_path(), "main.orna");
    assert!(loaded.modules()[0].source.contains("1 / 0"));

    let identity = RuntimeIdentity {
        database_id: [1; 16],
        repository_id: [2; 16],
    };
    let home = host_route(
        directory.path(),
        identity,
        false,
        &Request {
            method: "GET".into(),
            path: "/".into(),
            query: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
        },
    );
    assert_eq!(home.status, 404, "no native root page without serve");
    assert!(!String::from_utf8_lossy(&home.body).contains("No commits in this repository."));
    let query = host_route(
        directory.path(),
        identity,
        false,
        &Request {
            method: "POST".into(),
            path: "/api/query".into(),
            query: String::new(),
            headers: Vec::new(),
            body: b"1 + 1".to_vec(),
        },
    );
    assert_eq!(query.status, 404);
}

#[test]
fn content_route_serves_published_and_committed_bytes() {
    use orna_repository::{
        ManagedFileChange, ManagedPath, PublicationJournal, PublicationJournalEntry,
    };

    let directory = tempfile::tempdir().expect("temporary database");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=publish"],
    );
    // Commits made by the publication path need an identity that does not
    // depend on the host's global Git configuration.
    git_succeeds(directory.path(), &["config", "user.name", "kierandrewett"]);
    git_succeeds(
        directory.path(),
        &["config", "user.email", "kieran@drewett.dev"],
    );
    std::fs::write(directory.path().join("main.orna"), SERVE_FIXTURE)
        .expect("crate-local Orna source fixture");
    git_succeeds(directory.path(), &["add", "--", "main.orna"]);
    git_succeeds(
        directory.path(),
        &[
            "-c",
            "user.name=kierandrewett",
            "-c",
            "user.email=kieran@drewett.dev",
            "commit",
            "--quiet",
            "-m",
            "base",
        ],
    );

    let repository = Repository::discover(directory.path()).expect("database repository");
    let head = repository.head().expect("read HEAD").expect("base commit");
    let index_before = repository.index_generation().expect("index generation");
    let artifact = ManagedPath::new("published/capture.orna").unwrap();
    let candidate = repository
        .build_private_commit(
            &head,
            &[ManagedFileChange::new(
                artifact.clone(),
                Some(SERVE_FIXTURE.as_bytes().to_vec()),
            )],
            "orna: publish capture artifact",
        )
        .expect("private candidate commit");
    let mut journal = PublicationJournal::new_with_runtime_intent(
        head.clone(),
        candidate.commit().clone(),
        index_before.tree().expect("index tree").clone(),
        [3; 16],
        vec![PublicationJournalEntry::new(
            artifact.clone(),
            None,
            Some(SERVE_FIXTURE.as_bytes().to_vec()),
        )],
    )
    .expect("publication journal");
    repository
        .publish_candidate(&index_before, &candidate, &mut journal)
        .expect("publish capture artifact");
    let published = repository
        .head()
        .expect("read published HEAD")
        .expect("published commit");
    let published_id = published.as_str();

    let published = content_page(
        directory.path(),
        &format!("/content/{published_id}/published/capture.orna"),
    );
    assert_eq!(published.status, 200);
    assert_eq!(published.body, SERVE_FIXTURE.as_bytes());
}

#[test]
fn default_content_route_reads_committed_files_from_git() {
    let directory = tempfile::tempdir().expect("temporary database");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=listing"],
    );
    std::fs::create_dir_all(directory.path().join("src")).expect("source directory");
    std::fs::write(directory.path().join("main.orna"), SERVE_FIXTURE)
        .expect("crate-local Orna source fixture");
    std::fs::write(directory.path().join("src/nested.orna"), SERVE_FIXTURE)
        .expect("nested Orna source fixture");
    std::fs::write(directory.path().join("README.txt"), "committed text\n")
        .expect("generic text file");
    std::fs::write(directory.path().join("<b>&'\"x.txt"), "hostile name")
        .expect("hostile file name");
    std::fs::write(directory.path().join("src/data.bin"), [0, 255]).expect("generic binary file");
    git_succeeds(
        directory.path(),
        &[
            "add",
            "--",
            "main.orna",
            "src",
            "README.txt",
            "<b>&'\"x.txt",
        ],
    );
    git_succeeds(
        directory.path(),
        &[
            "-c",
            "user.name=kierandrewett",
            "-c",
            "user.email=kieran@drewett.dev",
            "commit",
            "--quiet",
            "-m",
            "render <listing> safely",
        ],
    );
    git_succeeds(directory.path(), &["branch", "archive"]);
    std::fs::write(directory.path().join("uncommitted.txt"), "not in Git yet")
        .expect("uncommitted worktree file");
    let repository = Repository::discover(directory.path()).expect("database repository");
    let commit = repository
        .head()
        .expect("read HEAD")
        .expect("listing commit");
    let commit_id = commit.as_str();

    let root = content_page(directory.path(), "/");
    assert_eq!(root.status, 404, "no native root page without serve");

    let tree = content_page(directory.path(), &format!("/tree/{commit_id}/"));
    assert_eq!(tree.status, 404, "the native tree listing is removed");

    let blob = content_page(directory.path(), &format!("/blob/{commit_id}/README.txt"));
    assert_eq!(blob.status, 404, "the native blob listing is removed");
    assert_eq!(
        blob.content_type, "application/json",
        "a removed listing returns no HTML page"
    );
    assert!(
        !String::from_utf8_lossy(&blob.body).contains("<html"),
        "a removed listing must not render document markup"
    );

    let nested = content_page(
        directory.path(),
        &format!("/content/{commit_id}/src/nested.orna"),
    );
    assert_eq!(nested.status, 200);
    assert_eq!(nested.body, SERVE_FIXTURE.as_bytes());

    let text = content_page(
        directory.path(),
        &format!("/content/{commit_id}/README.txt"),
    );
    assert_eq!(text.status, 200);
    assert_eq!(text.body, b"committed text\n");

    let hostile = content_page(
        directory.path(),
        &format!("/content/{commit_id}/%3Cb%3E%26%27%22x.txt"),
    );
    assert_eq!(hostile.status, 200);
    assert_eq!(hostile.body, b"hostile name");

    let binary = content_page(
        directory.path(),
        &format!("/content/{commit_id}/src/data.bin"),
    );
    assert_eq!(binary.status, 200);
    assert_eq!(binary.body, [0, 255]);

    let uncommitted = content_page(
        directory.path(),
        &format!("/content/{commit_id}/uncommitted.txt"),
    );
    assert_eq!(uncommitted.status, 404);
}

/// ORNA-SERVE-003: A different clone running `orna serve` serves that clone's HEAD and CWD; CWD MUST remain local to that clone.
#[test]
fn clone_route_reports_head_and_cwd_from_the_selected_clone() {
    let left = tempfile::tempdir().expect("left clone");
    let right = tempfile::tempdir().expect("right clone");
    for (directory, branch) in [(&left, "clone-left"), (&right, "clone-right")] {
        assert!(
            Command::new("git")
                .args(["init", "--quiet", "--initial-branch", branch])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.name=kierandrewett",
                    "-c",
                    "user.email=kieran@drewett.dev",
                    "commit",
                    "--quiet",
                    "--allow-empty",
                    "-m",
                    branch,
                ])
                .current_dir(directory.path())
                .status()
                .expect("git commit")
                .success()
        );
    }
    std::fs::write(right.path().join("local.txt"), "only in right").expect("right CWD change");
    let left_report = String::from_utf8(
        clone_report(
            left.path(),
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
        )
        .expect("left report"),
    )
    .expect("left JSON");
    let right_report = String::from_utf8(
        clone_report(
            right.path(),
            RuntimeIdentity {
                database_id: [3; 16],
                repository_id: [4; 16],
            },
        )
        .expect("right report"),
    )
    .expect("right JSON");
    let left_head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(left.path())
        .output()
        .expect("left HEAD")
        .stdout;
    let right_head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(right.path())
        .output()
        .expect("right HEAD")
        .stdout;
    let left_head = String::from_utf8(left_head).expect("left HEAD UTF-8");
    let right_head = String::from_utf8(right_head).expect("right HEAD UTF-8");
    assert!(left_report.contains("clone-left"));
    assert!(left_report.contains("\"worktree_clean\":true"));
    assert!(right_report.contains("clone-right"));
    assert!(right_report.contains("\"worktree_clean\":false"));
    assert!(left_report.contains(left_head.trim()));
    assert!(right_report.contains(right_head.trim()));
    assert!(!left_report.contains(right_head.trim()));
    assert!(!right_report.contains(left_head.trim()));
}

/// A13-live-serving-24: the request head is parsed once, so a request
/// line split across TCP segments still reaches the WebSocket route.
#[test]
fn request_head_reader_accepts_a_fragmented_request_line() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accepted socket");
        read_request_head(
            &mut BufReader::new(stream),
            Instant::now() + StdDuration::from_secs(5),
            &mut false,
        )
    });
    let mut client = TcpStream::connect(address).expect("loopback connection");
    client
        .write_all(b"GET /orna/live/")
        .expect("first fragment");
    std::thread::sleep(StdDuration::from_millis(10));
    client
        .write_all(b"session HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .expect("request line remainder");
    let (request, body_length) = server
        .join()
        .expect("request head thread")
        .expect("complete request head");
    assert_eq!(request.path, "/orna/live/session");
    assert_eq!(body_length, 0);
    assert!(is_websocket_route(&request));
}

/// ORNA-WIRE-005, A13-live-serving-24: a peer that never ends its request
/// line is refused at the header byte bound instead of being buffered or
/// polled until it stops.
#[test]
fn request_head_reader_refuses_an_unterminated_line_at_the_byte_bound() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accepted socket");
        let started = Instant::now();
        let result = read_request_head(
            &mut BufReader::new(stream),
            Instant::now() + StdDuration::from_secs(30),
            &mut false,
        );
        (result.err(), started.elapsed())
    });
    let mut client = TcpStream::connect(address).expect("loopback connection");
    client
        .write_all(&vec![b'a'; MAX_HEADER_BYTES + 1])
        .expect("unterminated request line");
    let (error, elapsed) = server.join().expect("request head thread");
    let error = error.expect("an unterminated line is refused");
    assert_ne!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        elapsed < StdDuration::from_secs(10),
        "the byte bound applies before the deadline: {elapsed:?}"
    );
}

/// ORNA-WIRE-005, A13-live-serving-24: a peer that trickles bytes without
/// ending its request line is cut off at the head deadline.
#[test]
fn request_head_reader_stops_a_trickling_peer_at_its_deadline() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
    let address = listener.local_addr().expect("listener address");
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("accepted socket");
        let started = Instant::now();
        let result = read_request_head(
            &mut BufReader::new(stream),
            Instant::now() + StdDuration::from_millis(300),
            &mut false,
        );
        (result.err(), started.elapsed())
    });
    let mut client = TcpStream::connect(address).expect("loopback connection");
    let trickle = std::thread::spawn(move || {
        for _ in 0..100 {
            if client.write_all(b"G").is_err() {
                break;
            }
            std::thread::sleep(StdDuration::from_millis(20));
        }
    });
    let (error, elapsed) = server.join().expect("request head thread");
    let error = error.expect("a trickling peer is refused");
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert!(
        elapsed >= StdDuration::from_millis(300) && elapsed < StdDuration::from_secs(2),
        "the head deadline bounds the whole request line: {elapsed:?}"
    );
    trickle.join().expect("trickling client");
}

/// ORNA-TRUST-002: `orna serve` binds to loopback by default. Remote deployment SHOULD use normal SSH, Tailscale, or a trusted authenticated reverse proxy/TLS boundary.
/// ORNA-SERVE-004: The default listener MUST bind only to loopback unless the operator explicitly selects another address.
#[test]
fn default_loopback_listener_serves_the_clone_route() {
    let directory = tempfile::tempdir().expect("temporary clone");
    assert!(
        Command::new("git")
            .args(["init", "--quiet", "--initial-branch", "listener-test"])
            .current_dir(directory.path())
            .status()
            .expect("git")
            .success()
    );
    let identity = RuntimeIdentity {
        database_id: [5; 16],
        repository_id: [6; 16],
    };
    let listener = LiveTransport::bind_default_listener(0).expect("loopback listener");
    let status = listener.status();
    assert!(status.address.ip().is_loopback());
    let root = directory.path().to_path_buf();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.listener().accept().expect("accepted request");
        let state = new_serve_state(
            test_clone(&root, identity),
            status.address,
            ServeOptions::default(),
        )
        .expect("serve host");
        serve_connection(stream, &state).expect("served clone route");
    });

    let mut client = TcpStream::connect(status.address).expect("loopback connection");
    client
        .write_all(
            format!(
                "GET /api/clone HTTP/1.1\r\nHost: {}\r\n\r\n",
                status.address
            )
            .as_bytes(),
        )
        .expect("request bytes");
    let mut response = Vec::new();
    client.read_to_end(&mut response).expect("response bytes");
    server.join().expect("server thread");
    let response = String::from_utf8(response).expect("HTTP response");
    assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(response.contains("\"HEAD\":null"));
    assert!(response.contains("\"CWD\":{\"branch\":\"listener-test\""));
}

#[allow(clippy::too_many_lines)]
#[test]
fn smart_http_clones_and_pushes_the_selected_clones_head() {
    for (index, branch, dirty) in [(0_u8, "http-left", false), (1_u8, "http-right", true)] {
        let source = tempfile::tempdir().expect("source clone");
        std::fs::write(source.path().join("main.orna"), SERVE_FIXTURE)
            .expect("vendored project source");
        assert!(
            Command::new("git")
                .args(["init", "--quiet", "--initial-branch", branch])
                .current_dir(source.path())
                .status()
                .expect("git init")
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.name=kierandrewett",
                    "-c",
                    "user.email=kieran@drewett.dev",
                    "add",
                    "main.orna",
                ])
                .current_dir(source.path())
                .status()
                .expect("git add")
                .success()
        );
        assert!(
            Command::new("git")
                .args([
                    "-c",
                    "user.name=kierandrewett",
                    "-c",
                    "user.email=kieran@drewett.dev",
                    "commit",
                    "--quiet",
                    "-m",
                    branch,
                ])
                .current_dir(source.path())
                .status()
                .expect("git commit")
                .success()
        );
        if dirty {
            std::fs::write(source.path().join("local-only.txt"), "clone-local CWD")
                .expect("local CWD change");
        }
        let expected_head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(source.path())
            .output()
            .expect("source HEAD")
            .stdout;
        let expected_head = String::from_utf8(expected_head)
            .expect("source HEAD UTF-8")
            .trim()
            .to_owned();

        let identity = RuntimeIdentity {
            database_id: [index + 10; 16],
            repository_id: [index + 20; 16],
        };
        let address = start_live_server(
            source.path(),
            identity,
            ServeOptions {
                allow_push: true,
                ..ServeOptions::default()
            },
        );

        let destination = tempfile::tempdir().expect("client clone parent");
        let clone_path = destination.path().join("checkout");
        let remote = format!("http://{address}/git");
        let cloned = Command::new("git")
            .args(["-c", "http.proxy=", "clone", "--quiet", &remote])
            .arg(&clone_path)
            .output()
            .expect("git clone");
        assert!(
            cloned.status.success(),
            "Git clone failed: {}",
            String::from_utf8_lossy(&cloned.stderr)
        );
        let cloned_head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&clone_path)
            .output()
            .expect("cloned HEAD")
            .stdout;
        assert_eq!(
            String::from_utf8(cloned_head).unwrap().trim(),
            expected_head
        );
        let pushed = Command::new("git")
            .args([
                "-c",
                "http.proxy=",
                "-C",
                clone_path.to_str().expect("clone path UTF-8"),
                "push",
                "--quiet",
                &remote,
                "HEAD:refs/heads/served-client",
            ])
            .output()
            .expect("git push");
        assert!(
            pushed.status.success(),
            "Git push failed: {}",
            String::from_utf8_lossy(&pushed.stderr)
        );
        let published_head = Command::new("git")
            .args(["rev-parse", "refs/heads/served-client"])
            .current_dir(source.path())
            .output()
            .expect("published HEAD")
            .stdout;
        assert_eq!(
            String::from_utf8(published_head).unwrap().trim(),
            expected_head
        );

        let report = exchange(
            address,
            &format!("GET /api/clone HTTP/1.1\r\nHost: {address}\r\n\r\n"),
        );
        assert_eq!(report.status, 200);
        assert!(
            report
                .body
                .contains(&format!("\"HEAD\":\"{expected_head}\""))
        );
        assert!(report.body.contains(&format!("\"branch\":\"{branch}\"")));
        assert!(
            report
                .body
                .contains(&format!("\"worktree_clean\":{}", !dirty))
        );
    }
}

/// ORNA-TRUST-003, A13-live-serving-09: smart-HTTP push changes refs in
/// the served clone, so it is refused unless the operator enables it.
/// Fetch stays available.
#[test]
fn smart_http_push_is_refused_unless_enabled() {
    let source = tempfile::tempdir().expect("source clone");
    std::fs::write(source.path().join("main.orna"), SERVE_FIXTURE)
        .expect("vendored project source");
    git_succeeds(
        source.path(),
        &["init", "--quiet", "--initial-branch=push-off"],
    );
    git_succeeds(source.path(), &["add", "main.orna"]);
    git_succeeds(
        source.path(),
        &[
            "-c",
            "user.name=Orna test",
            "-c",
            "user.email=orna-test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "base",
        ],
    );
    let address = start_live_server(source.path(), TEST_IDENTITY, ServeOptions::default());
    let remote = format!("http://{address}/git");
    let destination = tempfile::tempdir().expect("client clone parent");
    let clone_path = destination.path().join("checkout");
    let cloned = Command::new("git")
        .args(["-c", "http.proxy=", "clone", "--quiet", &remote])
        .arg(&clone_path)
        .output()
        .expect("git clone");
    assert!(
        cloned.status.success(),
        "fetch stays available: {}",
        String::from_utf8_lossy(&cloned.stderr)
    );
    let pushed = Command::new("git")
        .args([
            "-c",
            "http.proxy=",
            "-C",
            clone_path.to_str().expect("clone path UTF-8"),
            "push",
            "--quiet",
            &remote,
            "HEAD:refs/heads/refused",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git push");
    assert!(
        !pushed.status.success(),
        "push succeeded without `serve --allow-push`"
    );
    let created = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", "refs/heads/refused"])
        .current_dir(source.path())
        .output()
        .expect("refused ref lookup");
    assert!(!created.status.success(), "a refused push created a ref");
    let advertisement = exchange(
        address,
        &format!("GET /git/info/refs?service=git-receive-pack HTTP/1.1\r\nHost: {address}\r\n\r\n"),
    );
    assert_eq!(advertisement.status, 403);
    assert_eq!(advertisement.body, r#"{"error":"push_disabled"}"#);
}

/// ORNA-WIRE-011, ORNA-TRUST-003, A13-live-serving-09: normal same-origin
/// checks. Only this listener's loopback names are served, so a rebound
/// DNS name cannot reach it as a same-origin peer, and writes must come
/// from the request's own origin.
#[test]
fn requests_must_name_this_listener_and_writes_must_be_same_origin() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=hosts"],
    );
    let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
    let port = address.port();
    let own = address.to_string();
    let rebound = format!("rebound.example:{port}");
    let home = |host: &str| exchange(address, &format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n"));
    for host in [
        own.clone(),
        format!("localhost:{port}"),
        format!("LOCALHOST:{port}"),
        format!("[::1]:{port}"),
    ] {
        let accepted = home(&host);
        assert_eq!(accepted.status, 404, "{host} names this listener");
        assert_eq!(accepted.body, r#"{"error":"not_found"}"#);
    }
    for host in [
        rebound.clone(),
        format!("127.0.0.1:{}", port.wrapping_add(1)),
        "127.0.0.1".to_owned(),
        "localhost".to_owned(),
    ] {
        let refused = home(&host);
        assert_eq!(refused.status, 403, "{host} does not name this listener");
        assert_eq!(refused.body, r#"{"error":"host_denied"}"#);
    }

    let refused = create_session_as(address, &rebound, Some(&format!("http://{rebound}")));
    assert_eq!(refused.status, 403);
    assert!(
        refused.body.contains(r#""code":"live.host_denied""#),
        "{}",
        refused.body
    );
    for origin in [
        None,
        Some("null".to_owned()),
        Some("http://rebound.example".to_owned()),
        Some(format!("http://localhost:{port}")),
    ] {
        let refused = create_session_as(address, &own, origin.as_deref());
        assert_eq!(
            refused.status, 403,
            "{origin:?} is not this request's origin"
        );
        assert!(
            refused.body.contains(r#""code":"live.origin_denied""#),
            "{}",
            refused.body
        );
    }
    // Each loopback name of this listener is its own origin.
    for host in [own.clone(), format!("localhost:{port}")] {
        let created = create_session_as(address, &host, Some(&format!("http://{host}")));
        assert_eq!(created.status, 201, "{host}: {}", created.body);
    }

    // Git's own client sends no Origin; a page from another origin cannot
    // drive Git's POST endpoints.
    let upload = exchange(
        address,
        &format!(
            "POST /git/git-upload-pack HTTP/1.1\r\nHost: {own}\r\nOrigin: http://rebound.example\r\nContent-Type: application/x-git-upload-pack-request\r\nContent-Length: 0\r\n\r\n"
        ),
    );
    assert_eq!(upload.status, 403);
    assert_eq!(upload.body, r#"{"error":"origin_denied"}"#);

    // The refusal is decided from the request head alone: a cross-origin
    // request that declares a large body and never sends it is answered at
    // once, without the host waiting for or buffering that body.
    let started = Instant::now();
    let declared = exchange(
        address,
        &format!(
            "POST /git/git-upload-pack HTTP/1.1\r\nHost: {own}\r\nOrigin: http://rebound.example\r\nContent-Type: application/x-git-upload-pack-request\r\nContent-Length: 1000000\r\n\r\n"
        ),
    );
    assert_eq!(declared.status, 403);
    assert_eq!(declared.body, r#"{"error":"origin_denied"}"#);
    assert!(
        started.elapsed() < StdDuration::from_secs(5),
        "the refusal does not wait for the declared body: {:?}",
        started.elapsed()
    );

    // A WebSocket handshake through a rebound name never reaches the
    // session layer.
    let session = create_session(address);
    let upgrade = exchange(
        address,
        &format!(
            "GET /orna/live/{} HTTP/1.1\r\nHost: {rebound}\r\nOrigin: http://{rebound}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: orna.present.v1\r\nCookie: orna_session={}\r\n\r\n",
            session.id, session.token
        ),
    );
    assert_eq!(upgrade.status, 403);
    assert!(upgrade.body.contains(r#""code":"live.host_denied""#));
}

/// ORNA-WIRE-005, A13-live-serving-01: an attached WebSocket waiting on
/// its peer must not hold the shared session state. Before per-step
/// locking, the first socket's thread held it until that socket closed,
/// so the session request and the second socket below never completed.
#[test]
fn an_open_websocket_leaves_session_requests_and_other_sockets_available() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=live"],
    );
    let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
    let first = create_session(address);
    let mut first_socket = open_websocket(address, &first);

    let second = create_session(address);
    let mut second_socket = open_websocket(address, &second);
    assert_expression_result(&eval_over_socket(
        &mut second_socket,
        &second,
        [41; 16],
        EXPRESSION,
    ));
    assert_expression_result(&eval_over_socket(
        &mut first_socket,
        &first,
        [42; 16],
        EXPRESSION,
    ));
}

/// 30-protocol §Session creation and ownership, A13-live-serving-01: a
/// successful resume replaces the old attachment. The replaced socket's
/// worker is cancelled and acknowledges its retirement when it exits, so
/// the session is not left fenced against deletion.
#[test]
fn resume_replaces_the_attached_socket_and_releases_its_worker() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=resume"],
    );
    let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
    let session = create_session(address);
    let mut replaced = open_websocket(address, &session);

    let resumed = resume_session(address, &session);
    assert_eq!(resumed.id, session.id);
    assert_ne!(resumed.token, session.token);
    // The cancelled worker sends its closing handshake, then disconnects.
    expect_server_close(&mut replaced, CLOSE_RETIRED);

    let mut current = open_websocket(address, &resumed);
    send_frame(&mut current, 8, &1000_u16.to_be_bytes());
    let (opcode, payload) = read_frame(&mut current).expect("close acknowledgement");
    assert_eq!(opcode, 8);
    assert_eq!(payload, 1000_u16.to_be_bytes());
    assert_eq!(delete_session_once_released(address, &resumed), 204);
}

/// ORNA-WIRE-005, A13-live-serving-01: a half-open peer cannot hold its
/// attachment forever. An idle socket is pinged; a peer that answers
/// stays attached past the idle bound, and one that falls silent is
/// disconnected and its attachment released.
#[test]
fn idle_websockets_are_pinged_and_silent_peers_are_released() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=keepalive"],
    );
    // The idle bound leaves each ping more than two seconds for its
    // answer, so a test thread descheduled for a second on a loaded CI
    // runner is not mistaken for a silent peer.
    let keepalive = WebSocketKeepalive {
        ping_after: StdDuration::from_millis(100),
        idle_timeout: StdDuration::from_millis(2_500),
        write_timeout: StdDuration::from_secs(5),
    };
    let address = start_live_server(
        directory.path(),
        TEST_IDENTITY,
        ServeOptions {
            keepalive,
            ..ServeOptions::default()
        },
    );
    let session = create_session(address);
    let mut socket = open_websocket(address, &session);

    let answering = Instant::now();
    let mut pings = 0;
    while answering.elapsed() < keepalive.idle_timeout + keepalive.idle_timeout / 2 {
        let (opcode, payload) = read_frame(&mut socket).expect("keepalive ping");
        assert_eq!(opcode, 9, "an idle socket is pinged");
        send_frame(&mut socket, 10, &payload);
        pings += 1;
    }
    assert!(pings > 1, "a peer that answers pings stays attached");

    let (opcode, _) = read_frame(&mut socket).expect("final keepalive ping");
    assert_eq!(opcode, 9);
    // A silent peer is told the host is going away, then disconnected.
    expect_server_close(&mut socket, CLOSE_IDLE);
    assert_eq!(delete_session_once_released(address, &session), 204);
}

/// ORNA-WIRE-005, A13-live-serving-01: the keepalive judges the peer, not
/// the host. An evaluation that outlasts the idle bound (here, waiting on
/// the application lock) delivers its result and leaves the socket
/// attached for the next request.
#[test]
fn evaluation_time_is_not_counted_as_peer_silence() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=busy"],
    );
    // The idle bound leaves the client two seconds to send its next
    // request, so a test thread descheduled for a second is not mistaken
    // for a silent peer. The evaluation below is held for longer than it.
    let keepalive = WebSocketKeepalive {
        ping_after: StdDuration::from_millis(100),
        idle_timeout: StdDuration::from_secs(2),
        write_timeout: StdDuration::from_secs(5),
    };
    let (address, state) = start_shared_live_server(
        directory.path(),
        TEST_IDENTITY,
        ServeOptions {
            keepalive,
            ..ServeOptions::default()
        },
    );
    let session = create_session(address);
    let mut socket = open_websocket(address, &session);

    let busy = state.application.lock().expect("live application");
    let waiting = std::thread::spawn(move || {
        let result = eval_over_socket(&mut socket, &session, [51; 16], EXPRESSION);
        (socket, session, result)
    });
    std::thread::sleep(keepalive.idle_timeout + StdDuration::from_secs(1));
    drop(busy);
    let (mut socket, session, result) = waiting.join().expect("Eval client");
    assert_expression_result(&result);
    assert_expression_result(&eval_over_socket(
        &mut socket,
        &session,
        [52; 16],
        EXPRESSION,
    ));
}

/// 30-protocol §Session creation and ownership, A13-live-serving-15:
/// DELETE ends a session whose socket is still attached and which owns
/// application work. It cancels the socket's worker, which sends its
/// closing handshake and disconnects, and answers 204 only after that
/// worker has exited. Before, DELETE answered 503 while a socket was
/// attached and 400 once the session had run an evaluation.
#[test]
fn delete_ends_an_attached_session_after_its_socket_worker_exits() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=delete"],
    );
    let (address, state) =
        start_shared_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
    let session = create_session(address);
    let mut socket = open_websocket(address, &session);
    // The result also shows that the attachment has been committed.
    assert_expression_result(&eval_over_socket(
        &mut socket,
        &session,
        [61; 16],
        EXPRESSION,
    ));

    let deleted = delete_session(address, &session);
    assert_eq!(deleted.status, 204, "{}", deleted.body);
    assert!(
        state.workers.lock().is_empty(),
        "the socket's worker exits before the 204 is written"
    );
    expect_server_close(&mut socket, CLOSE_RETIRED);
    assert_eq!(resume_reply(address, &session).status, 410);
}

/// 30-protocol §Session creation and ownership, W20-04: a session that
/// has run an evaluation can be deleted once its socket has gone. DELETE
/// joins its finished application work and answers 204; before, it
/// answered 400 `live.malformed_request`.
#[test]
fn delete_ends_a_detached_session_that_has_evaluated() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=detached"],
    );
    let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
    let session = create_session(address);
    let mut socket = open_websocket(address, &session);
    assert_expression_result(&eval_over_socket(
        &mut socket,
        &session,
        [81; 16],
        EXPRESSION,
    ));
    send_frame(&mut socket, 8, &1000_u16.to_be_bytes());
    let (opcode, _) = read_frame(&mut socket).expect("close acknowledgement");
    assert_eq!(opcode, 8);
    assert!(closed_by_server(&mut socket));

    assert_eq!(delete_session_once_released(address, &session), 204);
    assert_eq!(resume_reply(address, &session).status, 410);
}

/// 30-protocol §Session creation and ownership, A13-live-serving-15:
/// expiry terminates the owner and cancels its children. Once a session's
/// lease elapses, the owner sweep ends it without any client request, so
/// its attached socket is closed and its token can no longer resume it.
#[test]
fn expired_sessions_are_ended_by_the_owner_sweep() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=expiry"],
    );
    // Keepalive pings start only after 15 s and the client waits at most
    // LIVE_TEST_TIMEOUT for the Close frame. The reconnection lease starts
    // when the socket detaches (30-protocol), so the session is ended only
    // after the client closes; the lease leaves the evaluation four seconds.
    let address = start_live_server(
        directory.path(),
        TEST_IDENTITY,
        ServeOptions {
            session_lease_ms: 4_000,
            ..ServeOptions::default()
        },
    );
    let session = create_session(address);
    let mut socket = open_websocket(address, &session);
    assert_expression_result(&eval_over_socket(
        &mut socket,
        &session,
        [71; 16],
        EXPRESSION,
    ));
    // Closing the socket detaches it, starting the reconnection lease.
    send_frame(&mut socket, 8, &1000_u16.to_be_bytes());
    let (opcode, _) = read_frame(&mut socket).expect("close acknowledgement");
    assert_eq!(opcode, 8);
    let deadline = Instant::now() + LIVE_TEST_TIMEOUT;
    loop {
        let reply = resume_reply(address, &session);
        if reply.status == 410 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the owner sweep ends the detached session"
        );
        std::thread::sleep(StdDuration::from_millis(50));
    }
    assert_eq!(resume_reply(address, &session).status, 410);
}

/// ORNA-WIRE-005, A13-live-serving-13: the connections being served,
/// each on its own thread, are bounded. Beyond the cap the accept loop
/// answers 503 at once without reading the request, and a connection's
/// slot is free again once it has ended.
#[test]
fn connections_beyond_the_cap_are_refused_until_one_ends() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=cap"],
    );
    let address = start_live_server(
        directory.path(),
        TEST_IDENTITY,
        ServeOptions {
            max_connections: 1,
            ..ServeOptions::default()
        },
    );
    let home = || {
        exchange(
            address,
            &format!("GET / HTTP/1.1\r\nHost: {address}\r\n\r\n"),
        )
    };
    // A request's slot is released before its peer sees the response end,
    // so the handshake that follows on a new connection is admitted.
    let session = create_session(address);
    let mut socket = open_websocket(address, &session);

    let refused = home();
    assert_eq!(refused.status, 503);
    assert_eq!(refused.body, r#"{"error":"temporarily_unavailable"}"#);

    send_frame(&mut socket, 8, &1000_u16.to_be_bytes());
    let (opcode, _) = read_frame(&mut socket).expect("close acknowledgement");
    assert_eq!(opcode, 8);
    assert!(closed_by_server(&mut socket));
    // The socket's worker ends the connection just before its thread
    // releases the slot.
    let deadline = Instant::now() + LIVE_TEST_TIMEOUT;
    loop {
        let reply = home();
        if reply.status != 503 {
            assert_eq!(reply.status, 404, "{}", reply.body);
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the ended connection's slot is released"
        );
        std::thread::sleep(StdDuration::from_millis(20));
    }
}

/// 30-protocol:15: the listener is plaintext HTTP, so the session cookie
/// must not carry `Secure`, which a browser would not return over http.
#[test]
fn session_cookie_is_not_secure_over_plain_http() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=cookie"],
    );
    let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
    let reply = create_session_as(
        address,
        &address.to_string(),
        Some(&format!("http://{address}")),
    );
    assert_eq!(reply.status, 201, "{}", reply.body);
    let cookie = reply
        .head
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
        .expect("session cookie");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(!cookie.contains("Secure"), "{cookie}");
}

/// 30-protocol:21: a connection refused by the cap on a `/orna/` route is
/// answered in the protocol's `{ "code", "message" }` shape.
#[test]
fn over_cap_refusal_on_orna_routes_uses_the_protocol_error_shape() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=refusal"],
    );
    let address = start_live_server(
        directory.path(),
        TEST_IDENTITY,
        ServeOptions {
            max_connections: 1,
            ..ServeOptions::default()
        },
    );
    // The open socket holds the only slot for as long as it stays attached.
    let session = create_session(address);
    let _socket = open_websocket(address, &session);

    let refused = exchange(
        address,
        &format!(
            "GET /orna/live/{} HTTP/1.1\r\nHost: {address}\r\n\r\n",
            session.id
        ),
    );
    assert_eq!(refused.status, 503);
    assert_eq!(
        refused.body,
        r#"{"code":"live.unavailable","message":"request rejected"}"#
    );
}

/// A silent peer refused over the cap must not delay the next refusal:
/// the accept loop answers the second peer without waiting out the first
/// peer's head timeout.
#[test]
fn silent_refused_peer_does_not_delay_the_next_refusal() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=silent"],
    );
    let address = start_live_server(
        directory.path(),
        TEST_IDENTITY,
        ServeOptions {
            max_connections: 1,
            ..ServeOptions::default()
        },
    );
    let session = create_session(address);
    let _socket = open_websocket(address, &session);
    // Sends nothing, so its refusal runs until REFUSAL_HEAD_TIMEOUT.
    let _silent = std::net::TcpStream::connect(address).expect("silent peer");

    let started = Instant::now();
    let refused = exchange(
        address,
        &format!(
            "GET /orna/live/{} HTTP/1.1\r\nHost: {address}\r\n\r\n",
            session.id
        ),
    );
    assert_eq!(refused.status, 503);
    assert!(
        started.elapsed() < REFUSAL_HEAD_TIMEOUT,
        "a silent peer delayed the next refusal"
    );
}

/// 30-protocol:21: once every refusal thread is busy, a connection over
/// the cap is still answered from the accept loop. A `/orna/` route gets
/// the protocol error shape, and a generic route gets the generic 503,
/// rather than a silent close.
#[test]
fn saturated_refusal_still_answers_with_the_route_shape() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=saturated"],
    );
    let address = start_live_server(
        directory.path(),
        TEST_IDENTITY,
        ServeOptions {
            max_connections: 1,
            ..ServeOptions::default()
        },
    );
    let session = create_session(address);
    let _socket = open_websocket(address, &session);
    // Silent peers fill every refusal thread; each holds its thread until
    // REFUSAL_HEAD_TIMEOUT. They are accepted in connect order, so the
    // probes below arrive after the pool is full.
    let _silent: Vec<std::net::TcpStream> = (0..MAX_REFUSALS_IN_FLIGHT)
        .map(|_| std::net::TcpStream::connect(address).expect("silent peer"))
        .collect();

    let refused = exchange(
        address,
        &format!(
            "GET /orna/live/{} HTTP/1.1\r\nHost: {address}\r\n\r\n",
            session.id
        ),
    );
    assert_eq!(refused.status, 503);
    assert_eq!(
        refused.body,
        r#"{"code":"live.unavailable","message":"request rejected"}"#
    );

    let generic = exchange(
        address,
        &format!("GET / HTTP/1.1\r\nHost: {address}\r\n\r\n"),
    );
    assert_eq!(generic.status, 503);
    assert_eq!(generic.body, r#"{"error":"temporarily_unavailable"}"#);
}

/// ORNA-WIRE-005: a request body must arrive within one transfer budget
/// measured over the whole body. A peer that trickles a declared body a
/// byte at a time is answered 400 at the deadline, instead of keeping its
/// connection thread for as long as it keeps sending.
#[test]
fn a_trickled_request_body_is_cut_off_at_its_deadline() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=trickle"],
    );
    let transfer = TransferBudget {
        base: StdDuration::from_millis(500),
        min_bytes_per_second: u64::MAX,
    };
    let address = start_live_server(
        directory.path(),
        TEST_IDENTITY,
        ServeOptions {
            transfer,
            ..ServeOptions::default()
        },
    );
    let started = Instant::now();
    let mut client = TcpStream::connect(address).expect("loopback connection");
    client
        .set_read_timeout(Some(LIVE_TEST_TIMEOUT))
        .expect("client read timeout");
    write!(
            client,
            "POST /orna/session HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n"
        )
        .expect("request head");
    // At one byte every 100 ms the body would take ten seconds.
    let mut trickle = client.try_clone().expect("second client handle");
    let trickler = std::thread::spawn(move || {
        for _ in 0..100 {
            if trickle.write_all(b" ").is_err() {
                break;
            }
            std::thread::sleep(StdDuration::from_millis(100));
        }
    });
    let response = read_until_closed(&mut client);
    let elapsed = started.elapsed();
    assert!(
        response.starts_with(b"HTTP/1.1 400 "),
        "{}",
        String::from_utf8_lossy(&response)
    );
    assert!(
        elapsed >= transfer.base && elapsed < StdDuration::from_secs(5),
        "the deadline bounds the whole body: {elapsed:?}"
    );
    trickler.join().expect("trickling client");
}

/// ORNA-WIRE-005: a response must be taken within one transfer budget
/// measured over the whole response. A client that stops reading a large
/// page cannot keep its connection thread: the server gives up at the
/// deadline, and the client later finds the page cut short.
#[test]
fn a_response_the_client_stops_reading_is_abandoned_at_its_deadline() {
    // The writer is under test. No route reaches this size now: the
    // committed-file route caps its reads at MAX_LISTING_FILE_BYTES.
    let response = Response::new(200, "text/plain; charset=utf-8", vec![b'"'; 12_000_000]);
    let total = response_head(&response).len() + response.body.len();
    assert!(
        total > 10_000_000,
        "the response outgrows the socket buffers"
    );
    let transfer = TransferBudget {
        base: StdDuration::from_millis(500),
        min_bytes_per_second: u64::MAX,
    };
    let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    let address = listener.local_addr().expect("listener address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accepted connection");
        write_response(&mut stream, response, transfer)
    });
    let mut client = TcpStream::connect(address).expect("loopback connection");
    client
        .set_read_timeout(Some(LIVE_TEST_TIMEOUT))
        .expect("client read timeout");
    // Read a little, then stop reading until well past the deadline.
    let mut first = [0; 1024];
    client.read_exact(&mut first).expect("first bytes");
    std::thread::sleep(transfer.base * 4);
    let received = first.len() + read_until_closed(&mut client).len();
    let outcome = server.join().expect("server thread");
    assert_eq!(
        outcome.expect_err("the writer gives up").kind(),
        io::ErrorKind::TimedOut
    );
    assert!(
        received < total,
        "the server stops writing at its deadline: {received} of {total} bytes"
    );
}

/// ORNA-EVAL-006 and the 30-protocol message registry ("A result for
/// success carries the complete typed value, including Unit"),
/// A13-live-serving-03: a REPL declaration over `orna serve` is answered
/// with typed Unit, and its binding persists for the session. Before, the
/// declaration's result was a bare null that could not be encoded, so the
/// client lost its answer while the server kept the binding.
#[test]
fn repl_declarations_over_serve_return_unit_and_persist() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=declarations"],
    );
    let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
    let session = create_session(address);
    let mut socket = open_websocket(address, &session);
    assert_success_value(
        &eval_over_socket(&mut socket, &session, [91; 16], "let x = 41;"),
        &CanonicalValue::unit(),
    );
    assert_success_value(
        &eval_over_socket(&mut socket, &session, [92; 16], "x + 1"),
        &int_value(42),
    );
}

/// ORNA-EVAL-006, ORNA-APP-010 and 30-protocol ("An eval source is
/// exactly one REPL input"), A14-04: console input that declares
/// `fn serve` is an ordinary REPL declaration. Before, `orna serve`
/// admitted any Eval declaring a function named `serve` as a one-shot
/// module and ran it, so the declaration answered with the function's
/// result and was never retained by the session.
#[test]
fn console_fn_serve_is_an_ordinary_declaration() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=console-serve"],
    );
    let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
    let session = create_session(address);
    let mut socket = open_websocket(address, &session);
    assert_success_value(
        &eval_over_socket(&mut socket, &session, [93; 16], "fn serve(): Int = 1;"),
        &CanonicalValue::unit(),
    );
    assert_success_value(
        &eval_over_socket(&mut socket, &session, [94; 16], "serve()"),
        &int_value(1),
    );
}

/// 30-protocol message registry, type 2 ("Host replies with the latest
/// full snapshot"; "explicit resync receives a snapshot with its
/// originating request ID"), ORNA-LIVE-004, A13-live-serving-08, and
/// ORNA-APP-017: an explicit resync is answered with the complete current
/// tree under its own request ID, and the console's scratch bindings are
/// not part of the page a watch evaluates. The page is pinned to the
/// admitted application module, so rebinding the console's own `x` leaves
/// the snapshot at revision 0, while the console still reads `x`.
#[test]
fn resync_repeats_the_pinned_page_when_the_console_rebinds_its_name() {
    let directory = tempfile::tempdir().expect("temporary clone");
    git_succeeds(
        directory.path(),
        &["init", "--quiet", "--initial-branch=resync"],
    );
    let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
    let session = create_session(address);
    let mut socket = open_websocket(address, &session);
    assert_success_value(
        &eval_over_socket(&mut socket, &session, [95; 16], "let x = 41;"),
        &CanonicalValue::unit(),
    );
    let watched = watch_over_socket(&mut socket, &session, [96; 16], "1 + 1");
    assert_eq!(watched.request, Some([96; 16]));
    let watch = watched.watch.expect("a watch snapshot carries its handle");
    let Message::Snapshot {
        revision: 0,
        present,
        ..
    } = watched.message
    else {
        panic!("a new watch starts with a revision-zero snapshot");
    };
    assert_eq!(
        present,
        orna_protocol::PresentNode::from_value(int_value(2)).expect("watched tree")
    );

    assert_success_value(
        &eval_over_socket(&mut socket, &session, [97; 16], "let x = 1;"),
        &CanonicalValue::unit(),
    );
    assert_success_value(
        &eval_over_socket(&mut socket, &session, [99; 16], "x"),
        &int_value(1),
    );
    let refreshed = resync_over_socket(&mut socket, &session, [98; 16], watch);
    assert_eq!(
        refreshed.request,
        Some([98; 16]),
        "the snapshot names its resync"
    );
    assert_eq!(refreshed.watch, Some(watch));
    let Message::Snapshot {
        revision, present, ..
    } = refreshed.message
    else {
        panic!("an explicit resync is answered with a full snapshot");
    };
    assert_eq!(
        revision, 0,
        "a console binding does not move the page revision"
    );
    assert_eq!(
        present,
        orna_protocol::PresentNode::from_value(int_value(2)).expect("refreshed tree")
    );
}

/// G-4: the served runtime is opened under this process's owner, the same
/// owner `orna run`, the REPL and mounts use in this process. It holds the
/// worktree's runtime owner lock and the writer lease for that owner at
/// epoch 1. Re-acquisition is idempotent, and a different owner is refused
/// even through the served runtime that holds the lock.
#[test]
fn served_runtime_holds_the_process_owner_lease() {
    let directory = tempfile::tempdir().expect("temporary project");
    orna_repository::initialize_repository(directory.path()).expect("orna init");
    std::fs::write(
        directory.path().join("main.orna"),
        "pub table Note(id: Int) { text: Str, }\n",
    )
    .expect("project source");
    let endpoint = Endpoint::Path(
        directory
            .path()
            .to_str()
            .expect("UTF-8 project path")
            .to_owned(),
    );
    let clone = open_served_clone(&endpoint).expect("`orna serve` resolves the clone");
    let owner = invocation_owner_id();
    let executor = current_thread_executor().expect("test executor");
    let lease = executor
        .block_on(clone.runtime.acquire_lease(owner))
        .expect("the repository owner acquires the served lease");
    assert_eq!(lease.owner_id, owner);
    assert_eq!(lease.epoch, 1);
    assert!(clone.runtime.holds_runtime_owner_lock());
    assert_eq!(
        executor
            .block_on(clone.runtime.acquire_lease(owner))
            .expect("the holder re-acquires its own lease"),
        lease
    );
    assert!(matches!(
        executor.block_on(clone.runtime.acquire_lease([7; 16])),
        Err(orna_runtime::RuntimeError::LeaseHeld)
    ));
    assert_eq!(
        executor
            .block_on(clone.runtime.current_lease())
            .expect("read the served lease"),
        Some(lease)
    );
}

/// ORNA-EVAL-003, ORNA-EVAL-008, ORNA-PROTO-004 and 30-protocol REQUEST-1
/// (A13-live-serving-04): `orna serve` hosts the clone's durable runtime.
/// An Eval that inserts a row into a project table commits it once,
/// together with its terminal outcome, and resending the same request ID
/// returns that recorded outcome without writing again. Before, the live
/// host had no runtime state and refused every mutating Eval.
#[test]
fn served_eval_insert_commits_once_and_replays_its_recorded_outcome() {
    let directory = tempfile::tempdir().expect("temporary project");
    orna_repository::initialize_repository(directory.path()).expect("orna init");
    std::fs::write(
        directory.path().join("main.orna"),
        "pub table Note(id: Int) { text: Str, }\n",
    )
    .expect("project source");
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
    let insert = r#"main.Note.insert({ id: 1, text: "served" })"#;
    let row = CanonicalValue::new(OvbRaw::Map(vec![
        (OvbRaw::Text("id".into()), OvbRaw::Int(1.into())),
        (OvbRaw::Text("text".into()), OvbRaw::Text("served".into())),
    ]))
    .expect("canonical row");

    let committed = eval_over_socket(&mut socket, &session, [99; 16], insert);
    assert_success_value(&committed, &row);
    let rows = committed_rows(directory.path(), "Note");
    assert_eq!(rows.len(), 1, "the insert commits exactly one row");
    assert_eq!(
        CanonicalValue::decode(&rows[0].1).expect("committed row value"),
        row
    );

    let replayed = eval_over_socket(&mut socket, &session, [99; 16], insert);
    assert_eq!(
        replayed, committed,
        "the same request ID returns its recorded outcome"
    );
    assert_eq!(
        committed_rows(directory.path(), "Note"),
        rows,
        "the replay does not write again"
    );
}

/// ORNA-A091-009 and 30-protocol forbidden effect (A13-live-serving-04): an
/// Eval whose table commit fails the table assertion is refused with a
/// correlated diagnostic, writes no row, and leaves the socket open, so the
/// next request on it commits as usual.
#[test]
fn served_eval_refused_by_table_assertion_reports_its_diagnostic() {
    let directory = tempfile::tempdir().expect("temporary project");
    orna_repository::initialize_repository(directory.path()).expect("orna init");
    std::fs::write(
        directory.path().join("main.orna"),
        "pub table Book(id: Int) {\n    title: Str,\n    \
             assert every(row => row.title != \"\");\n}\n",
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

    let refused = eval_over_socket(
        &mut socket,
        &session,
        [41; 16],
        r#"main.Book.insert({ id: 1, title: "" })"#,
    );
    let Message::Diagnostic { diagnostic, .. } = &refused.message else {
        panic!("Eval answers with a Diagnostic, not {refused:?}");
    };
    assert_eq!(refused.request, Some([41; 16]), "the refusal is correlated");
    assert_eq!(
        diagnostic.code().expect("a portable diagnostic code"),
        "ORNA-A091-009"
    );
    assert!(
        committed_rows(directory.path(), "Book").is_empty(),
        "a refused write commits no row"
    );

    let row = CanonicalValue::new(OvbRaw::Map(vec![
        (OvbRaw::Text("id".into()), OvbRaw::Int(2.into())),
        (OvbRaw::Text("title".into()), OvbRaw::Text("Dune".into())),
    ]))
    .expect("canonical row");
    let admitted = eval_over_socket(
        &mut socket,
        &session,
        [42; 16],
        r#"main.Book.insert({ id: 2, title: "Dune" })"#,
    );
    assert_success_value(&admitted, &row);
    assert_eq!(
        committed_rows(directory.path(), "Book").len(),
        1,
        "the admitted write commits exactly one row"
    );
}

/// Commits the clone's `main.orna` with the identity inventory `orna
/// commit` stages for it, so the served host reads each live table's
/// committed identity from HEAD. The sources are committed first, as the
/// LSP fixtures do, and the identities follow in a second commit.
fn commit_project_with_identities(root: &Path) {
    git_succeeds(root, &["add", "main.orna", ".orna/database.orna"]);
    git_commit(root, "base");
    let source = std::fs::read_to_string(root.join("main.orna")).expect("project source");
    let module = orna_semantic::ModuleInput::new("main.orna", source.as_str());
    let identities =
        orna_project::ObjectIdentityManifest::reconcile(std::slice::from_ref(&module), None)
            .expect("object identities");
    let repository = Repository::discover(root).expect("served clone");
    let staged = repository
        .capture_staged_candidate()
        .expect("staged candidate");
    repository
        .stage_object_identities(&staged, &identities.to_stored(), &[])
        .expect("stage object identities");
    git_commit(root, "identities");
}

fn git_commit(directory: &Path, message: &str) {
    git_succeeds(
        directory,
        &[
            "-c",
            "user.name=orna-test",
            "-c",
            "user.email=orna-test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "-m",
            message,
        ],
    );
}

/// The committed rows of `table`, read through a second handle on the
/// clone's runtime state, as another local command would read them.
fn committed_rows(root: &Path, table: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    let repository = Repository::discover(root).expect("served clone");
    let (identity, initial_digest) = runtime_identity(&repository).expect("clone runtime identity");
    current_thread_executor()
        .expect("runtime executor")
        .block_on(async {
            RuntimeState::open(&repository, identity, initial_digest)
                .await
                .expect("reopen the clone runtime")
                .committed_table_rows(table)
                .await
                .expect("committed rows")
        })
}

const LIVE_TEST_TIMEOUT: StdDuration = StdDuration::from_secs(10);
const TEST_IDENTITY: RuntimeIdentity = RuntimeIdentity {
    database_id: [31; 16],
    repository_id: [32; 16],
};
/// Initial runtime digest of a test clone served under a fixed identity.
const TEST_INITIAL_DIGEST: [u8; 32] = [33; 32];
/// A pure console expression, answered by [`assert_expression_result`].
const EXPRESSION: &str = "40 + 2";

/// The Git worktree at `root` as `orna serve` hosts it, under a fixed
/// identity, with the core catalogue and no loaded project.
fn test_clone(root: &Path, identity: RuntimeIdentity) -> ServedClone {
    let repository = Repository::discover(root).expect("served Git worktree");
    let runtime = current_thread_executor()
        .expect("runtime executor")
        .block_on(RuntimeState::open(
            &repository,
            identity,
            TEST_INITIAL_DIGEST,
        ))
        .expect("clone runtime state");
    ServedClone {
        root: root.to_path_buf(),
        identity,
        catalogue: orna_semantic::Catalogue::authoritative_core(),
        project: None,
        app: None,
        runtime,
        table_candidate_validator: None,
    }
}

/// Starts the production accept loop on its own thread, exactly as
/// `orna serve` runs it. The thread lives until the test process exits.
fn start_live_server(root: &Path, identity: RuntimeIdentity, options: ServeOptions) -> SocketAddr {
    start_shared_live_server(root, identity, options).0
}

/// [`start_live_server`], also returning the state the listener shares
/// with its connection threads.
fn start_shared_live_server(
    root: &Path,
    identity: RuntimeIdentity,
    options: ServeOptions,
) -> (SocketAddr, Arc<ServeState>) {
    serve_clone(test_clone(root, identity), options)
}

/// Serves `clone` from the production accept loop on its own thread and
/// returns its address and the state the listener shares with its
/// connection threads.
fn serve_clone(clone: ServedClone, options: ServeOptions) -> (SocketAddr, Arc<ServeState>) {
    let listener = LiveTransport::bind_default_listener(0).expect("loopback listener");
    let address = listener.status().address;
    let state = Arc::new(new_serve_state(clone, address, options).expect("serve host"));
    let shared = Arc::clone(&state);
    std::thread::spawn(move || serve_listener(listener.listener(), &shared));
    (address, state)
}

struct Reply {
    status: u16,
    head: String,
    body: String,
}

fn exchange(address: SocketAddr, request: &str) -> Reply {
    let mut stream = TcpStream::connect(address).expect("loopback connection");
    stream
        .set_read_timeout(Some(LIVE_TEST_TIMEOUT))
        .expect("client read timeout");
    stream.write_all(request.as_bytes()).expect("request bytes");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("the server answers before the client timeout");
    let response = String::from_utf8(response).expect("UTF-8 response");
    let (head, body) = response
        .split_once("\r\n\r\n")
        .expect("complete response head");
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("response status");
    Reply {
        status,
        head: head.to_owned(),
        body: body.to_owned(),
    }
}

struct LiveSession {
    id: String,
    token: String,
    database: [u8; 16],
}

fn post_json(
    address: SocketAddr,
    host: &str,
    origin: Option<&str>,
    path: &str,
    body: &str,
) -> Reply {
    let origin = origin
        .map(|origin| format!("Origin: {origin}\r\n"))
        .unwrap_or_default();
    exchange(
        address,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\n{origin}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
}

fn create_session_as(address: SocketAddr, host: &str, origin: Option<&str>) -> Reply {
    create_database_session_as(address, host, origin, TEST_IDENTITY.database_id)
}

fn create_database_session_as(
    address: SocketAddr,
    host: &str,
    origin: Option<&str>,
    database: [u8; 16],
) -> Reply {
    post_json(
        address,
        host,
        origin,
        "/orna/session",
        &format!(
            r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
            format_uuid(database)
        ),
    )
}

fn live_session(reply: &Reply) -> LiveSession {
    let body: serde_json::Value = serde_json::from_str(&reply.body).expect("session JSON");
    LiveSession {
        id: body["session"].as_str().expect("session id").to_owned(),
        token: body["resume_token"]
            .as_str()
            .expect("resume token")
            .to_owned(),
        database: uuid_bytes(body["database"].as_str().expect("session database")),
    }
}

fn create_session(address: SocketAddr) -> LiveSession {
    create_session_in(address, TEST_IDENTITY.database_id)
}

fn create_session_in(address: SocketAddr, database: [u8; 16]) -> LiveSession {
    let host = address.to_string();
    let reply =
        create_database_session_as(address, &host, Some(&format!("http://{host}")), database);
    assert_eq!(reply.status, 201, "{}", reply.body);
    live_session(&reply)
}

fn resume_reply(address: SocketAddr, session: &LiveSession) -> Reply {
    let host = address.to_string();
    post_json(
        address,
        &host,
        Some(&format!("http://{host}")),
        &format!("/orna/session/{}/resume", session.id),
        &format!(
            r#"{{"resume_token":"{}","protocol":"orna.present.v1"}}"#,
            session.token
        ),
    )
}

fn resume_session(address: SocketAddr, session: &LiveSession) -> LiveSession {
    let reply = resume_reply(address, session);
    assert_eq!(reply.status, 200, "{}", reply.body);
    live_session(&reply)
}

fn delete_session(address: SocketAddr, session: &LiveSession) -> Reply {
    let host = address.to_string();
    exchange(
        address,
        &format!(
            "DELETE /orna/session/{} HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\nAuthorization: Bearer {}\r\n\r\n",
            session.id, session.token
        ),
    )
}

/// Deletion answers 503 while a retired socket worker of the session has
/// not yet acknowledged its retirement, so retry until it settles.
fn delete_session_once_released(address: SocketAddr, session: &LiveSession) -> u16 {
    let deadline = Instant::now() + LIVE_TEST_TIMEOUT;
    loop {
        let status = delete_session(address, session).status;
        if status != 503 || Instant::now() >= deadline {
            return status;
        }
        std::thread::sleep(StdDuration::from_millis(20));
    }
}

fn open_websocket(address: SocketAddr, session: &LiveSession) -> TcpStream {
    let mut stream = TcpStream::connect(address).expect("loopback connection");
    stream
        .set_read_timeout(Some(LIVE_TEST_TIMEOUT))
        .expect("client read timeout");
    write!(
            stream,
            "GET /orna/live/{} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: orna.present.v1\r\nCookie: orna_session={}\r\n\r\n",
            session.id, session.token
        )
        .expect("upgrade request");
    let mut head = Vec::new();
    let mut byte = [0; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream
            .read_exact(&mut byte)
            .expect("upgrade response before the client timeout");
        head.push(byte[0]);
    }
    let head = String::from_utf8(head).expect("UTF-8 upgrade response");
    assert!(head.starts_with("HTTP/1.1 101 "), "upgrade refused: {head}");
    stream
}

fn send_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) {
    let mask = [0x11, 0x22, 0x33, 0x44];
    let mut frame = vec![0x80 | opcode];
    if let Ok(length) = u8::try_from(payload.len())
        && length < 126
    {
        frame.push(0x80 | length);
    } else if let Ok(length) = u16::try_from(payload.len()) {
        frame.push(0x80 | 0x7e);
        frame.extend_from_slice(&length.to_be_bytes());
    } else {
        frame.push(0x80 | 0x7f);
        let length = u64::try_from(payload.len()).expect("frame length");
        frame.extend_from_slice(&length.to_be_bytes());
    }
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4]),
    );
    stream.write_all(&frame).expect("client frame");
}

fn read_frame(stream: &mut TcpStream) -> io::Result<(u8, Vec<u8>)> {
    let mut head = [0; 2];
    stream.read_exact(&mut head)?;
    let length = match head[1] & 0x7f {
        126 => {
            let mut length = [0; 2];
            stream.read_exact(&mut length)?;
            usize::from(u16::from_be_bytes(length))
        }
        127 => {
            let mut length = [0; 8];
            stream.read_exact(&mut length)?;
            usize::try_from(u64::from_be_bytes(length)).expect("frame length")
        }
        length => usize::from(length),
    };
    let mut payload = vec![0; length];
    stream.read_exact(&mut payload)?;
    Ok((head[0] & 0x0f, payload))
}

/// Whether the server has closed the socket: end of stream or a reset,
/// but not a client read timeout.
fn closed_by_server(stream: &mut TcpStream) -> bool {
    let mut byte = [0; 1];
    match stream.read(&mut byte) {
        Ok(read) => read == 0,
        Err(error) => matches!(
            error.kind(),
            io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
        ),
    }
}

/// Reads until the server ends the connection, by end of stream or reset.
fn read_until_closed(stream: &mut TcpStream) -> Vec<u8> {
    let mut received = Vec::new();
    let mut chunk = vec![0; 64 * 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => return received,
            Ok(read) => received.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                ) =>
            {
                return received;
            }
            Err(error) => panic!("the server ends the connection in time: {error}"),
        }
    }
}

/// Reads the server's closing handshake: one Close frame carrying `code`,
/// then the end of the connection.
fn expect_server_close(stream: &mut TcpStream, code: u16) {
    let (opcode, payload) = read_frame(stream).expect("server Close frame");
    assert_eq!(opcode, 8, "the server sends a Close frame");
    assert_eq!(payload, code.to_be_bytes());
    assert!(
        closed_by_server(stream),
        "the server disconnects after its Close frame"
    );
}

fn uuid_bytes(text: &str) -> [u8; 16] {
    let digits = text.replace('-', "");
    let mut bytes = [0; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&digits[index * 2..index * 2 + 2], 16).expect("UUID hex");
    }
    bytes
}

/// Sends one client request over `stream` and returns the host's answer.
/// An Eval carries its request fingerprint, as a conforming client sends
/// it; other requests need not.
fn request_over_socket(
    stream: &mut TcpStream,
    session: &LiveSession,
    mut envelope: Envelope,
) -> Envelope {
    let limits = ProtocolLimits::default();
    let fingerprint =
        orna_protocol::canonical_request_fingerprint(uuid_bytes(&session.id), &envelope, limits)
            .expect("request fingerprint");
    if let Message::Eval {
        fingerprint: sent, ..
    } = &mut envelope.message
    {
        *sent = fingerprint;
    }
    send_frame(
        stream,
        2,
        &envelope.encode(limits).expect("canonical request"),
    );
    loop {
        let (opcode, payload) = read_frame(stream).expect("answer before the client timeout");
        // A conforming client answers keepalive pings while it waits.
        if opcode == 9 {
            send_frame(stream, 10, &payload);
            continue;
        }
        assert_eq!(
            opcode, 2,
            "the answer is one binary message, not opcode {opcode} with payload {payload:?}"
        );
        return Envelope::decode(&payload, limits).expect("canonical answer");
    }
}

fn session_database(session: &LiveSession) -> orna_protocol::DatabaseContext {
    orna_protocol::DatabaseContext {
        database: session.database,
        snapshot: None,
    }
}

fn terminal_presentation() -> PresentationContext {
    PresentationContext {
        locale: "en".into(),
        timezone: None,
        width: None,
        theme: "terminal/default".into(),
        supported_kinds: vec!["text".into(), "group".into()],
    }
}

fn eval_over_socket(
    stream: &mut TcpStream,
    session: &LiveSession,
    request: [u8; 16],
    source: &str,
) -> Envelope {
    request_over_socket(
        stream,
        session,
        Envelope {
            request: Some(request),
            watch: None,
            message: Message::Eval {
                source: source.to_owned(),
                database: session_database(session),
                presentation: terminal_presentation(),
                fingerprint: [0; 32],
            },
            extensions: BTreeMap::new(),
        },
    )
}

fn watch_over_socket(
    stream: &mut TcpStream,
    session: &LiveSession,
    request: [u8; 16],
    source: &str,
) -> Envelope {
    request_over_socket(
        stream,
        session,
        Envelope {
            request: Some(request),
            watch: None,
            message: Message::Watch {
                source: source.to_owned(),
                database: session_database(session),
                presentation: terminal_presentation(),
                refresh_floor: None,
            },
            extensions: BTreeMap::new(),
        },
    )
}

fn resync_over_socket(
    stream: &mut TcpStream,
    session: &LiveSession,
    request: [u8; 16],
    watch: [u8; 16],
) -> Envelope {
    request_over_socket(
        stream,
        session,
        Envelope {
            request: Some(request),
            watch: Some(watch),
            message: Message::Resync,
            extensions: BTreeMap::new(),
        },
    )
}

fn int_value(value: i64) -> CanonicalValue {
    CanonicalValue::new(OvbRaw::Int(value.into())).expect("canonical integer")
}

/// Asserts that `response` is a success result carrying `expected`.
fn assert_success_value(response: &Envelope, expected: &CanonicalValue) {
    let Message::Result {
        status,
        value,
        diagnostic,
        ..
    } = &response.message
    else {
        panic!("Eval answers with a Result, not {response:?}");
    };
    assert_eq!(
        *status,
        orna_protocol::ResultStatus::Success,
        "{diagnostic:?}"
    );
    assert_eq!(value.as_ref(), Some(expected));
}

fn assert_expression_result(response: &Envelope) {
    assert_success_value(response, &int_value(42));
}

#[test]
fn cgi_environment_drops_git_proxy_and_token_variables() {
    let mut command = Command::new("/usr/bin/env");
    command.env("GIT_DIR", "/should/not/leak");
    command.env("GIT_CONFIG_GLOBAL", "/should/not/leak");
    command.env("HTTPS_PROXY", "http://should-not-leak.invalid");
    command.env("ORNA_SERVER_TOKEN", "should-not-leak");
    command.env("XDG_CONFIG_HOME", "/should/not/leak");
    clear_cgi_environment(&mut command);
    let output = command.output().expect("env runs");
    assert!(output.status.success());
    let names: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_owned()))
        .collect();
    for name in &names {
        assert!(
            ["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR"].contains(&name.as_str()),
            "CGI child inherited unexpected variable {name}"
        );
    }
    for name in ["PATH", "HOME", "TMPDIR"] {
        if std::env::var_os(name).is_some() {
            assert!(names.iter().any(|kept| kept == name), "{name} was dropped");
        }
    }
}
