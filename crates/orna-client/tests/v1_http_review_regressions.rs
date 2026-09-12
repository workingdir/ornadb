//! Executable HTTP-boundary audit for Orna 1.0.0; no implementation fixes.
//!
//! Tracking: ornadb-gov5.17.1, https://github.com/workingdir/ornadb/issues/791.
//! Immutable requirements (paths relative to the coordination workspace):
//! - reference/Orna-1.0.0/source/30-protocol.md, "Session creation and ownership":
//!   UUID identities, a 32-byte unpadded Base64url token, and scoped credentials.
//! - reference/Orna-1.0.0/profiles/session.schema.json, $defs/session_response:
//!   exact hexadecimal 8-4-4-4-12 UUID groups; the top-level description also
//!   explicitly requires validation of token canonical bits.
//! - reference/Orna-1.0.0/source/14-pages.md, ORNA-WIRE-011: same-origin checks.
//!   Chapter 30, "Transport and value profile", permits explicitly trusted
//!   loopback without TLS. Different loopback ports still have different origins.
//!
//! Ignored tests assert required behavior and are expected to FAIL on the
//! reviewed implementation when explicitly run. An ignored result is not a
//! conformance pass. Ordinary tests are controls. All credentials are synthetic;
//! traffic stays on numeric IPv4 loopback. No temporary files are created.
//! The UUID panic check requires panic unwinding (the usual Rust test profile).
//! If proxy environment variables are set, both NO_PROXY and no_proxy must
//! explicitly bypass 127.0.0.1 (or all hosts) before these tests can run.

use std::{
    future::Future,
    io::ErrorKind,
    net::TcpListener as StdTcpListener,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use orna_client::{LiveClient, LiveClientConfig, LiveSession, LiveTransportError, TlsPolicy};
use orna_protocol_v1::Limits;
use reqwest::Url;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinError,
};

const SESSION: &str = "01010101-0101-0101-0101-010101010101";
const DATABASE: &str = "02020202-0202-0202-0202-020202020202";
const RUNTIME: &str = "abababab-abab-abab-abab-abababababab";
const PROTOCOL: &str = "orna.present.v1";
const BASE64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn run<F: Future>(future: F) -> F::Output {
    // LiveClient owns its HTTP builder, so use an environment precondition to
    // keep this public-API audit off external proxies without global mutation.
    let proxy_configured = [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    let loopback_bypassed = ["NO_PROXY", "no_proxy"].iter().all(|name| {
        std::env::var(name).is_ok_and(|value| {
            value
                .split(',')
                .any(|host| matches!(host.trim(), "127.0.0.1" | "*"))
        })
    });
    assert!(
        !proxy_configured || loopback_bypassed,
        "loopback audit requires absent proxy variables or both NO_PROXY=127.0.0.1 and no_proxy=127.0.0.1"
    );
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("create audit runtime")
        .block_on(async move {
            tokio::time::timeout(Duration::from_secs(30), future)
                .await
                .expect("loopback audit exceeded its safety deadline")
        })
}

fn client(endpoint: Url) -> LiveClient {
    LiveClient::new(LiveClientConfig {
        endpoint,
        origin: "http://localhost".into(),
        limits: Limits::default(),
        request_timeout: Duration::from_secs(5),
        tls_policy: TlsPolicy::TrustedLoopbackOnly,
    })
    .expect("configure trusted loopback client")
}

fn token(last: u8) -> String {
    format!("{}{last}", "A".repeat(42), last = char::from(last))
}

fn session_body(runtime: &str, resume_token: &str) -> String {
    json!({
        "session": SESSION,
        "database": DATABASE,
        "runtime": runtime,
        "resume_token": resume_token,
        "websocket_path": format!("/orna/live/{SESSION}"),
        "lease_ms": 30_000,
        "limits": {
            "max_message_bytes": 16_777_216,
            "max_depth": 64,
            "max_nodes": 100_000,
            "max_collection_items": 100_000,
            "max_outgoing_bytes": 16_777_216,
            "request_retention_ms": 30_000
        }
    })
    .to_string()
}

struct Request {
    head: String,
    body: Vec<u8>,
}

async fn read_request(stream: &mut TcpStream) -> Request {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        assert!(head.len() < 16_384, "audit request headers exceed bound");
        head.push(stream.read_u8().await.expect("read request header"));
    }
    let head = String::from_utf8(head).expect("HTTP request headers are UTF-8");
    // Production session POSTs use a known-size JSON body. Fail explicitly if
    // that framing changes; do not silently miss credentials in chunked data.
    assert!(
        !head.lines().any(|line| line
            .split_once(':')
            .is_some_and(|(name, _)| name.eq_ignore_ascii_case("transfer-encoding"))),
        "audit harness requires Content-Length framing"
    );
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("valid Content-Length"))
        })
        .unwrap_or(0);
    assert!(length <= 4096, "audit request body exceeds bound");
    let mut body = vec![0; length];
    stream
        .read_exact(&mut body)
        .await
        .expect("read request body");
    Request { head, body }
}

fn assert_post(request: &Request, path: &str, expected: Value) {
    assert!(
        request.head.lines().next() == Some(format!("POST {path} HTTP/1.1").as_str()),
        "request must use the specified session POST endpoint"
    );
    assert!(
        serde_json::from_slice::<Value>(&request.body).expect("request JSON") == expected,
        "session request must contain exactly the specified fields and values"
    );
}

async fn reply(stream: &mut TcpStream, status: &str, headers: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("write audit response");
    stream.shutdown().await.expect("finish audit response");
}

async fn reply_session(stream: &mut TcpStream, status: &str, body: &str, cookie: &str) {
    reply(
        stream,
        status,
        &format!(
            "Content-Type: application/json\r\nSet-Cookie: orna_session={cookie}; Path=/orna/live/{SESSION}; HttpOnly; SameSite=Strict\r\n"
        ),
        body,
    )
    .await;
}

async fn listener() -> (TcpListener, Url) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback origin");
    let endpoint = Url::parse(&format!(
        "http://{}/",
        listener.local_addr().expect("loopback address")
    ))
    .expect("loopback URL");
    (listener, endpoint)
}

async fn create_with_body(
    body: String,
) -> Result<Result<LiveSession, LiveTransportError>, JoinError> {
    let (listener, endpoint) = listener().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept create");
        assert_post(
            &read_request(&mut stream).await,
            "/orna/session",
            json!({"database": DATABASE, "protocol": PROTOCOL}),
        );
        reply_session(&mut stream, "201 Created", &body, "synthetic-create-cookie").await;
    });
    // A parser panic must fail the normative assertion, not become an accepted
    // error or an expected #[should_panic] result.
    let request = tokio::spawn(async move { client(endpoint).create_session([2; 16]).await });
    let result = request.await;
    server.await.expect("create server completed");
    result
}

fn assert_identity(session: &LiveSession) {
    assert_eq!(session.session_id(), [1; 16]);
    assert_eq!(session.database_id(), [2; 16]);
    assert_eq!(session.runtime_id(), [0xab; 16]);
}

#[test]
fn canonical_base64url_final_symbols_are_accepted() {
    run(async {
        // 32 bytes = 256 bits: the final sextet has four data bits and two
        // unused zero bits. Exactly indices divisible by four are canonical:
        // AEIMQUYcgkosw048. This oracle follows the encoding, not the parser.
        for (index, &symbol) in BASE64URL.iter().enumerate() {
            if index % 4 == 0 {
                let session = create_with_body(session_body(RUNTIME, &token(symbol)))
                    .await
                    .expect("canonical token must not panic")
                    .expect("canonical token must be accepted");
                assert_identity(&session);
            }
        }
    });
}

#[test]
fn uppercase_runtime_uuid_is_accepted() {
    run(async {
        let session = create_with_body(session_body(&RUNTIME.to_ascii_uppercase(), &token(b'A')))
            .await
            .expect("valid UUID must not panic")
            .expect("hexadecimal UUID may use uppercase letters");
        assert_identity(&session);
    });
}

#[test]
fn invalid_runtime_uuid_length_and_nonhex_are_rejected() {
    run(async {
        for runtime in ["short", "gggggggg-0000-0000-0000-000000000000"] {
            let result = create_with_body(session_body(runtime, &token(b'A'))).await;
            assert!(
                matches!(result, Ok(Err(LiveTransportError::Response(_)))),
                "invalid runtime UUID must return a response error without panicking"
            );
        }
    });
}

#[test]
#[ignore = "Known Orna 1.0.0 gap: #791; run explicitly for review"]
fn malformed_runtime_uuid_is_rejected_without_panicking() {
    run(async {
        let result = create_with_body(session_body(
            "00000000-0000-0000-0000--00000000000",
            &token(b'A'),
        ))
        .await;
        assert!(
            matches!(result, Ok(Err(LiveTransportError::Response(_)))),
            "extra UUID separator must return a response error without panicking"
        );
    });
}

#[test]
#[ignore = "Known Orna 1.0.0 gap: #791; run explicitly for review"]
fn all_hyphen_runtime_uuid_is_rejected() {
    run(async {
        let result = create_with_body(session_body(&"-".repeat(36), &token(b'A'))).await;
        assert!(
            matches!(result, Ok(Err(LiveTransportError::Response(_)))),
            "a UUID must contain exactly 32 hexadecimal digits and four separators"
        );
    });
}

#[test]
#[ignore = "Known Orna 1.0.0 gap: #791; run explicitly for review"]
fn noncanonical_base64url_final_symbols_are_rejected() {
    run(async {
        let mut violations = Vec::new();
        // Exercise all 48 forbidden suffixes, including 42 'A's followed by
        // 'B'; collect failures so one accepted suffix cannot mask the others.
        for (index, &symbol) in BASE64URL.iter().enumerate() {
            if index % 4 != 0 {
                let result = create_with_body(session_body(RUNTIME, &token(symbol))).await;
                if !matches!(result, Ok(Err(LiveTransportError::Response(_)))) {
                    violations.push(index);
                }
            }
        }
        assert!(
            violations.is_empty(),
            "nonzero unused token bits must return a response error; offending sextet indices: {violations:?}"
        );
    });
}

async fn resume_redirect_must_not_forward(status: &'static str) {
    // Retain a nonblocking clone to inspect the kernel accept queue after the
    // client finishes and the capture task is joined. This catches connections
    // even if that task was not scheduled; no arbitrary observation sleep.
    let pending = StdTcpListener::bind("127.0.0.1:0").expect("bind capture origin");
    pending.set_nonblocking(true).expect("nonblocking capture");
    let capture = TcpListener::from_std(pending.try_clone().expect("clone capture listener"))
        .expect("register capture listener");
    let destination = format!(
        "http://{}/capture",
        pending.local_addr().expect("capture address")
    );
    let contacted = Arc::new(AtomicBool::new(false));
    let forwarded = Arc::new(AtomicBool::new(false));
    let capture_task = {
        let contacted = Arc::clone(&contacted);
        let forwarded = Arc::clone(&forwarded);
        tokio::spawn(async move {
            let (mut stream, _) = capture.accept().await.expect("accept redirect capture");
            contacted.store(true, Ordering::SeqCst);
            let request = read_request(&mut stream).await;
            let credential = token(b'A');
            forwarded.store(
                request
                    .body
                    .windows(credential.len())
                    .any(|window| window == credential.as_bytes()),
                Ordering::SeqCst,
            );
            // Rejection by B does not undo disclosure of the JSON credential.
            reply(&mut stream, "400 Bad Request", "", "").await;
        })
    };
    let (origin, endpoint) = listener().await;
    assert_ne!(
        endpoint.port_or_known_default(),
        Url::parse(&destination).unwrap().port_or_known_default()
    );
    let server = tokio::spawn(async move {
        let (mut create, _) = origin.accept().await.expect("accept create");
        assert_post(
            &read_request(&mut create).await,
            "/orna/session",
            json!({"database": DATABASE, "protocol": PROTOCOL}),
        );
        reply_session(
            &mut create,
            "201 Created",
            &session_body(RUNTIME, &token(b'A')),
            "synthetic-create-cookie",
        )
        .await;
        let (mut resume, _) = origin.accept().await.expect("accept resume");
        assert_post(
            &read_request(&mut resume).await,
            &format!("/orna/session/{SESSION}/resume"),
            json!({"resume_token": token(b'A'), "protocol": PROTOCOL}),
        );
        reply(
            &mut resume,
            status,
            &format!("Location: {destination}\r\n"),
            "",
        )
        .await;
    });
    let client = client(endpoint);
    let session = client
        .create_session([2; 16])
        .await
        .expect("valid bootstrap");
    let result = client.resume_session(&session).await;
    server.await.expect("redirect origin completed");
    capture_task.abort();
    if let Err(error) = capture_task.await {
        assert!(error.is_cancelled(), "capture task must not panic");
    }
    match pending.accept() {
        Ok(_) => contacted.store(true, Ordering::SeqCst),
        Err(error) => assert_eq!(error.kind(), ErrorKind::WouldBlock),
    }
    assert!(
        result.is_err(),
        "cross-origin resume redirect must be rejected"
    );
    assert!(
        !forwarded.load(Ordering::SeqCst),
        "resume token must never be forwarded to the redirect origin"
    );
    assert!(
        !contacted.load(Ordering::SeqCst),
        "cross-origin resume redirect must be rejected before contacting its target"
    );
}

#[test]
#[ignore = "Known Orna 1.0.0 gap: #791; run explicitly for review"]
fn resume_307_redirect_is_rejected_without_forwarding_credentials() {
    run(resume_redirect_must_not_forward("307 Temporary Redirect"));
}

#[test]
#[ignore = "Known Orna 1.0.0 gap: #791; run explicitly for review"]
fn resume_308_redirect_is_rejected_without_forwarding_credentials() {
    run(resume_redirect_must_not_forward("308 Permanent Redirect"));
}

#[test]
fn same_origin_resume_with_rotated_credentials_succeeds() {
    run(async {
        let (listener, endpoint) = listener().await;
        let server = tokio::spawn(async move {
            for (path, expected, status, last, cookie) in [
                (
                    "/orna/session".to_owned(),
                    json!({"database": DATABASE, "protocol": PROTOCOL}),
                    "201 Created",
                    b'A',
                    "synthetic-create-cookie",
                ),
                (
                    format!("/orna/session/{SESSION}/resume"),
                    json!({"resume_token": token(b'A'), "protocol": PROTOCOL}),
                    "200 OK",
                    b'E',
                    "synthetic-rotated-cookie",
                ),
            ] {
                let (mut stream, _) = listener.accept().await.expect("accept session POST");
                assert_post(&read_request(&mut stream).await, &path, expected);
                reply_session(
                    &mut stream,
                    status,
                    &session_body(RUNTIME, &token(last)),
                    cookie,
                )
                .await;
            }
        });
        let client = client(endpoint);
        let session = client
            .create_session([2; 16])
            .await
            .expect("valid bootstrap");
        assert_identity(&session);
        let resumed = client.resume_session(&session).await.expect("valid resume");
        assert_identity(&resumed);
        server.await.expect("same-origin server completed");
    });
}
