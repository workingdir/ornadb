use futures::{
    executor::block_on,
    io::{AsyncRead, AsyncWrite, Cursor},
};
use orna_foundation_v1::CanonicalValue;
use orna_live_v1::{
    CreateRequest, DeleteRequest, Error, Frame, FrameOutcome, HttpBody, HttpConnection,
    HttpConnectionError, HttpEncodeError, HttpIoError, HttpParseError, Limits, LiveApplication,
    LiveCredentialIssuer, LiveHost, LiveSessionAuthority, LiveTransport, ResumeRequest,
    SUBPROTOCOL, SessionCredential, SessionMetadata, TransportLimits, WebSocketOutput,
    WebSocketState, WireRequest, WireResponse, encode_websocket_output, parse_http_request,
};
use orna_protocol_v1::{
    DatabaseContext, Envelope, Message, PresentationContext, ResultStatus, TargetKind,
    canonical_request_fingerprint,
};
use orna_repository_v1::Repository;
use orna_runtime_v1::{RequestIdentity, RuntimeIdentity, RuntimeState, TerminalOutcome};
use orna_security_v1::{
    AttachmentId, BoundaryError, CredentialIssuer, Origin, OriginPolicy, SessionBoundary,
    SessionDeletionAdapter,
};
use orna_serving_v1::{Limits as ServingLimits, Serving};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

struct FailFirstWriter {
    writes: usize,
}

#[derive(Default)]
struct RecordingWriter {
    bytes: Vec<u8>,
    flushes: usize,
    closes: usize,
}

impl AsyncWrite for RecordingWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.bytes.extend_from_slice(bytes);
        std::task::Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.flushes += 1;
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_close(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.closes += 1;
        std::task::Poll::Ready(Ok(()))
    }
}

struct PendingReader;

impl AsyncRead for PendingReader {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Pending
    }
}

struct PendingWriter;

impl AsyncWrite for PendingWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        _: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        context.waker().wake_by_ref();
        std::task::Poll::Pending
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        context.waker().wake_by_ref();
        std::task::Poll::Pending
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

struct CancelAfterPolls {
    polls: usize,
    ready_after: usize,
}

impl std::future::Future for CancelAfterPolls {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.polls += 1;
        if self.polls >= self.ready_after {
            std::task::Poll::Ready(())
        } else {
            context.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

impl AsyncWrite for FailFirstWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.writes += 1;
        std::task::Poll::Ready(if self.writes == 1 {
            Err(std::io::Error::other("write rejected"))
        } else {
            Ok(0)
        })
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

struct CountingAuthority {
    calls: usize,
    times: Vec<u64>,
}

impl LiveSessionAuthority for CountingAuthority {
    fn create_session(&mut self, database: [u8; 16], now: u64) -> Result<SessionMetadata, Error> {
        self.calls += 1;
        self.times.push(now);
        Ok(SessionMetadata {
            session: [u8::try_from(self.calls).expect("test call count fits"); 16],
            database,
            runtime: [3; 16],
            expires_at: now + 100,
            subscribe: subscribe(),
        })
    }
}

struct Issuer(u8, Option<[u8; 32]>);
impl CredentialIssuer for Issuer {
    fn issue_credential(&mut self) -> Result<[u8; 32], BoundaryError> {
        let credential = [self.0; 32];
        self.0 += 1;
        self.1 = Some(credential);
        Ok(credential)
    }
}
impl LiveCredentialIssuer for Issuer {
    fn last_issued(&self) -> Option<[u8; 32]> {
        self.1
    }
}
struct Delete(bool);
impl SessionDeletionAdapter for Delete {
    type Error = ();
    fn delete(&mut self, _: orna_security_v1::SessionId) -> Result<(), Self::Error> {
        self.0.then_some(()).ok_or(())
    }
}

#[derive(Default)]
struct UnitApplication {
    calls: usize,
    reject: bool,
    reject_cancel: bool,
}

fn unit_result(request: [u8; 16], fingerprint: [u8; 32]) -> Envelope {
    Envelope {
        request: Some(request),
        watch: None,
        message: Message::Result {
            status: ResultStatus::Success,
            value: Some(CanonicalValue::unit()),
            fingerprint,
            diagnostic: None,
        },
        extensions: BTreeMap::new(),
    }
}

impl LiveApplication for UnitApplication {
    fn eval(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> Result<Envelope, Error> {
        let Message::Eval { fingerprint, .. } = message else {
            return Err(Error::ApplicationRejected);
        };
        self.calls += 1;
        if self.reject {
            return Err(Error::ApplicationRejected);
        }
        Ok(unit_result(request, *fingerprint))
    }

    fn watch(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope, Error> {
        Err(Error::UnsupportedOperation)
    }

    fn cancel(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        fingerprint: [u8; 32],
        _: &Message,
    ) -> Result<Envelope, Error> {
        self.calls += 1;
        if self.reject_cancel {
            return Err(Error::ApplicationRejected);
        }
        Ok(unit_result(request, fingerprint))
    }
}

struct Authority;
impl LiveSessionAuthority for Authority {
    fn create_session(&mut self, database: [u8; 16], _: u64) -> Result<SessionMetadata, Error> {
        Ok(SessionMetadata {
            session: [1; 16],
            database,
            runtime: [3; 16],
            expires_at: 100,
            subscribe: subscribe(),
        })
    }
}

fn origin() -> Origin {
    Origin::parse("https://app.example").unwrap()
}
fn host() -> LiveHost {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    LiveHost::new(
        Limits::default(),
        boundary,
        Serving::new(ServingLimits::default()).unwrap(),
    )
    .unwrap()
}

fn durable_host(runtime: RuntimeState) -> LiveHost {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    LiveHost::with_runtime_state(
        Limits::default(),
        boundary,
        Serving::new(ServingLimits::default()).unwrap(),
        runtime,
    )
    .unwrap()
}

fn durable_repository() -> (PathBuf, Repository) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("orna-live-v1-{nonce}"));
    fs::create_dir(&root).unwrap();
    let status = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success());
    let repository = Repository::discover(&root).unwrap();
    (root, repository)
}

fn open_durable_state(repository: &Repository) -> RuntimeState {
    block_on(RuntimeState::open(
        repository,
        RuntimeIdentity {
            database_id: [6; 16],
            repository_id: [7; 16],
        },
        [8; 32],
    ))
    .unwrap()
}

fn request_fingerprint(bytes: &[u8], session: [u8; 16]) -> [u8; 32] {
    let envelope = Envelope::decode(bytes, Limits::default().protocol).unwrap();
    canonical_request_fingerprint(session, &envelope, Limits::default().protocol).unwrap()
}

fn remove_test_repository(root: &Path) {
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn http_decoder_handles_partial_body_and_pipelined_bytes() {
    let limits = TransportLimits::default();
    let request = b"POST /orna/session HTTP/1.1\r\nHost: example\r\nContent-Length: 4\r\n\r\nbodyGET /orna/session HTTP/1.1\r\n\r\n";
    assert_eq!(parse_http_request(&request[..68], limits).unwrap(), None);
    let parsed = parse_http_request(request, limits).unwrap().unwrap();
    assert_eq!(parsed.request().method, "POST");
    assert_eq!(parsed.request().path, "/orna/session");
    assert_eq!(parsed.request().body, b"body");
    assert_eq!(
        &request[parsed.consumed()..],
        b"GET /orna/session HTTP/1.1\r\n\r\n"
    );
}

#[test]
fn http_decoder_rejects_ambiguous_or_unsupported_framing() {
    let limits = TransportLimits::default();
    for raw in [
        b"POST /orna/session HTTP/1.1\r\nHost: example\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx"
            .as_slice(),
        b"POST /orna/session HTTP/1.1\r\nHost: example\r\nTransfer-Encoding: chunked\r\n\r\n"
            .as_slice(),
        b"GET /orna/session HTTP/1.0\r\nHost: example\r\n\r\n".as_slice(),
    ] {
        assert_eq!(
            parse_http_request(raw, limits),
            Err(HttpParseError::Malformed)
        );
    }
}

#[test]
fn http_decoder_applies_header_and_request_limits_before_body_materialisation() {
    let limits = TransportLimits {
        max_header_bytes: 32,
        ..TransportLimits::default()
    };
    assert_eq!(
        parse_http_request(
            b"GET /orna/session HTTP/1.1\r\nHost: example\r\n\r\n",
            limits
        ),
        Err(HttpParseError::Limit)
    );

    let limits = TransportLimits {
        max_request_bytes: 64,
        ..TransportLimits::default()
    };
    assert_eq!(
        parse_http_request(
            b"POST /orna/session HTTP/1.1\r\nHost: example\r\nContent-Length: 100\r\n\r\n",
            limits
        ),
        Err(HttpParseError::Limit)
    );
}

#[test]
fn http_response_encoder_serializes_bounded_responses_and_owns_content_length() {
    let response = WireResponse {
        status: 201,
        headers: vec![("content-type".into(), "application/json".into())],
        body: br#"{"ok":true}"#.to_vec(),
    };
    let encoded = response.encode_http(TransportLimits::default()).unwrap();
    assert_eq!(
        encoded,
        b"HTTP/1.1 201 Created\r\ncontent-type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}"
    );

    let limited = TransportLimits {
        max_outgoing_bytes: encoded.len() - 1,
        ..TransportLimits::default()
    };
    assert_eq!(response.encode_http(limited), Err(HttpEncodeError::Limit));

    for response in [
        WireResponse {
            status: 200,
            headers: vec![("Content-Length".into(), "1".into())],
            body: vec![b'x'],
        },
        WireResponse {
            status: 200,
            headers: vec![("x-test".into(), "ok\r\nInjected: yes".into())],
            body: Vec::new(),
        },
        WireResponse {
            status: 204,
            headers: Vec::new(),
            body: vec![b'x'],
        },
    ] {
        assert_eq!(
            response.encode_http(TransportLimits::default()),
            Err(HttpEncodeError::Malformed)
        );
    }
}

#[test]
fn http_connection_retains_partial_reads_and_drains_pipelined_requests() {
    let mut connection = HttpConnection::new(TransportLimits::default());
    let request = b"GET /orna/session HTTP/1.1\r\nHost: example\r\n\r\nGET /orna/session HTTP/1.1\r\nHost: example\r\n\r\n";
    assert!(connection.push(&request[..20]).unwrap().is_empty());
    assert_eq!(connection.buffered_bytes(), 20);
    let requests = connection.push(&request[20..]).unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].request().path, "/orna/session");
    assert_eq!(requests[1].request().method, "GET");
    assert_eq!(connection.buffered_bytes(), 0);
}

#[test]
fn http_connection_bounds_an_incomplete_request_before_append() {
    let limits = TransportLimits {
        max_request_bytes: 32,
        ..TransportLimits::default()
    };
    let mut connection = HttpConnection::new(limits);
    assert_eq!(
        connection.push(b"GET /orna/session HTTP/1.1\r\nHost: "),
        Err(HttpParseError::Limit)
    );
    assert_eq!(connection.buffered_bytes(), 0);
}

#[test]
fn http_connection_driver_routes_partial_reads_and_encodes_the_response() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let body = format!(
        r#"{{"database":"{}","protocol":"{}"}}"#,
        uuid(2),
        SUBPROTOCOL
    );
    let request = format!(
        "POST /orna/session HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let request_bytes = request.as_bytes();
    let split = request.len() / 2;
    let mut authority = Authority;
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    assert!(
        block_on(transport.handle_http_read(
            &mut connection,
            &request_bytes[..split],
            0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        ))
        .unwrap()
        .is_empty()
    );
    let responses = block_on(transport.handle_http_read(
        &mut connection,
        &request_bytes[split..],
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ))
    .unwrap();
    assert_eq!(responses.len(), 1);
    assert!(responses[0].starts_with(b"HTTP/1.1 201 Created\r\n"));
    assert!(
        responses[0]
            .windows(b"Content-Length: ".len())
            .any(|window| window == b"Content-Length: ")
    );
    assert_eq!(connection.buffered_bytes(), 0);
    let mut rejected_connection = HttpConnection::new(TransportLimits::default());
    assert!(matches!(
        block_on(transport.handle_http_read(
            &mut rejected_connection,
            b"GET /orna/session HTTP/1.1\r\n\r\n",
            0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        )),
        Err(HttpConnectionError::Parse(HttpParseError::Malformed))
    ));
}

#[test]
fn async_http_connection_loop_writes_routed_responses_until_eof() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let body = format!(
        r#"{{"database":"{}","protocol":"{}"}}"#,
        uuid(2),
        SUBPROTOCOL
    );
    let request = format!(
        "POST /orna/session HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let mut reader = Cursor::new(request.into_bytes());
    let mut writer = Cursor::new(Vec::new());
    let mut authority = Authority;
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    block_on(transport.serve_http_connection(
        &mut reader,
        &mut writer,
        &mut connection,
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ))
    .unwrap();
    assert!(writer.into_inner().starts_with(b"HTTP/1.1 201 Created\r\n"));
    assert_eq!(connection.buffered_bytes(), 0);
}

#[test]
fn accepted_tcp_socket_routes_a_session_request_end_to_end() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut authority = Authority;
        let mut issuer = Issuer(7, None);
        let mut deletion = Delete(true);
        transport.serve_accepted_http_socket(
            stream,
            &mut connection,
            &mut || 0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        )
    });

    let body = format!(
        r#"{{"database":"{}","protocol":"{}"}}"#,
        uuid(2),
        SUBPROTOCOL
    );
    let request = format!(
        "POST /orna/session HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let mut client = TcpStream::connect(address).unwrap();
    client.write_all(request.as_bytes()).unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).unwrap();
    server.join().unwrap().unwrap();

    assert!(response.starts_with(b"HTTP/1.1 201 Created\r\n"));
    assert!(response.windows(4).any(|window| window == b"\r\n\r\n"));
}

#[test]
fn accepted_tcp_socket_hands_off_an_upgrade_to_the_websocket_driver() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
        let mut authority = Authority;
        let mut issuer = Issuer(7, None);
        let mut deletion = Delete(true);
        let created = block_on(transport.handle(
            wire(
                "POST",
                "/orna/session",
                &format!(
                    r#"{{"database":"{}","protocol":"{}"}}"#,
                    uuid(2),
                    SUBPROTOCOL
                ),
            ),
            0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        ));
        sender.send(token(&created)).unwrap();
        let (stream, _) = listener.accept().unwrap();
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut application = UnitApplication::default();
        transport.serve_accepted_websocket_socket(
            stream,
            &mut connection,
            [5; 16],
            &mut || 1,
            &mut application,
        )
    });

    let request = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        receiver.recv().unwrap()
    );
    let mut client = TcpStream::connect(address).unwrap();
    client.write_all(request.as_bytes()).unwrap();
    client.write_all(&masked(true, 9, b"hi")).unwrap();
    client.write_all(&masked(true, 8, b"")).unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).unwrap();
    server.join().unwrap().unwrap();

    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("serialized handshake response");
    assert!(response.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
    assert_eq!(&response[header_end + 4..], b"\x8a\x02hi\x88\x00");
}

#[test]
fn async_http_connection_loop_rejects_truncated_eof() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut reader = Cursor::new(b"GET /orna/session HTTP/1.1\r\nHost: example\r\n".to_vec());
    let mut writer = Cursor::new(Vec::new());
    let mut authority = Authority;
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    assert_eq!(
        block_on(transport.serve_http_connection(
            &mut reader,
            &mut writer,
            &mut connection,
            0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        )),
        Err(HttpIoError::Transport(HttpConnectionError::Parse(
            HttpParseError::Incomplete
        )))
    );
    assert!(writer.into_inner().is_empty());
}

#[test]
fn async_http_connection_loop_writes_before_admitting_next_pipelined_request() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let body = format!(
        r#"{{"database":"{}","protocol":"{}"}}"#,
        uuid(2),
        SUBPROTOCOL
    );
    let request = format!(
        "POST /orna/session HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let mut reader = Cursor::new([request.as_bytes(), request.as_bytes()].concat());
    let mut writer = FailFirstWriter { writes: 0 };
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    assert_eq!(
        block_on(transport.serve_http_connection(
            &mut reader,
            &mut writer,
            &mut connection,
            0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        )),
        Err(HttpIoError::Write)
    );
    assert_eq!(authority.calls, 1);
}

#[test]
fn async_http_connection_loop_samples_clock_for_each_request() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let body = format!(
        r#"{{"database":"{}","protocol":"{}"}}"#,
        uuid(2),
        SUBPROTOCOL
    );
    let request = format!(
        "POST /orna/session HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let mut reader = Cursor::new([request.as_bytes(), request.as_bytes()].concat());
    let mut writer = Cursor::new(Vec::new());
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    let mut times = vec![17, 23];
    block_on(transport.serve_http_connection_with_clock(
        &mut reader,
        &mut writer,
        &mut connection,
        &mut || times.remove(0),
        &mut authority,
        &mut issuer,
        &mut deletion,
    ))
    .unwrap();
    assert_eq!(authority.times, [17, 23]);
}

#[test]
fn async_http_connection_loop_cancels_a_pending_read() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut reader = PendingReader;
    let mut writer = Cursor::new(Vec::new());
    let mut authority = Authority;
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    let mut clock = || 0;
    let mut cancellation = CancelAfterPolls {
        polls: 0,
        ready_after: 2,
    };
    assert_eq!(
        block_on(transport.serve_http_connection_with_cancellation(
            &mut reader,
            &mut writer,
            &mut connection,
            &mut clock,
            &mut cancellation,
            &mut authority,
            &mut issuer,
            &mut deletion,
        )),
        Err(HttpIoError::Cancelled)
    );
    assert!(writer.into_inner().is_empty());
}

#[test]
fn async_http_connection_loop_cancels_a_pending_write() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let body = format!(
        r#"{{"database":"{}","protocol":"{}"}}"#,
        uuid(2),
        SUBPROTOCOL
    );
    let request = format!(
        "POST /orna/session HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let mut reader = Cursor::new(request.into_bytes());
    let mut writer = PendingWriter;
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    let mut clock = || 0;
    let mut cancellation = CancelAfterPolls {
        polls: 0,
        ready_after: 3,
    };
    assert_eq!(
        block_on(transport.serve_http_connection_with_cancellation(
            &mut reader,
            &mut writer,
            &mut connection,
            &mut clock,
            &mut cancellation,
            &mut authority,
            &mut issuer,
            &mut deletion,
        )),
        Err(HttpIoError::Cancelled)
    );
    assert_eq!(authority.calls, 1);
}

#[test]
fn websocket_connection_driver_preserves_a_co_read_frame_after_upgrade() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let cookie = token(&created);
    let upgrade = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        cookie
    );
    let mut input = upgrade.into_bytes();
    input.extend(masked(true, 9, b"hi"));
    let mut reader = Cursor::new(input);
    let mut writer = Cursor::new(Vec::new());
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = std::future::pending::<()>();
    block_on(transport.serve_websocket_connection(
        &mut reader,
        &mut writer,
        &mut connection,
        [5; 16],
        &mut clock,
        &mut cancellation,
        &mut application,
    ))
    .unwrap();
    let output = writer.into_inner();
    let header_end = output
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("serialized handshake response");
    assert!(output.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
    assert!(
        !output[..header_end]
            .windows(16)
            .any(|window| window == b"Content-Length: ")
    );
    assert_eq!(&output[header_end + 4..], b"\x8a\x02hi");
}

#[test]
fn websocket_connection_driver_delivers_co_read_frames_before_admitting_the_next() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let input = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        token(&created)
    );
    let mut bytes = input.into_bytes();
    bytes.extend(masked(true, 9, b"one"));
    bytes.extend(masked(true, 9, b"two"));
    let mut reader = Cursor::new(bytes);
    let mut writer = RecordingWriter::default();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = std::future::pending::<()>();
    block_on(transport.serve_websocket_connection(
        &mut reader,
        &mut writer,
        &mut connection,
        [5; 16],
        &mut clock,
        &mut cancellation,
        &mut application,
    ))
    .unwrap();
    assert_eq!(writer.flushes, 3);
    let header_end = writer
        .bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    assert_eq!(&writer.bytes[header_end + 4..], b"\x8a\x03one\x8a\x03two");
}

#[test]
fn websocket_connection_driver_does_not_commit_an_upgrade_before_handshake_delivery() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let input = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        token(&created)
    );
    let request = parse_http_request(input.as_bytes(), TransportLimits::default())
        .unwrap()
        .unwrap()
        .request()
        .clone();
    assert_eq!(block_on(transport.upgrade(request, [4; 16], 1)).status, 101);
    let mut reader = Cursor::new(input.into_bytes());
    let mut writer = FailFirstWriter { writes: 0 };
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = std::future::pending::<()>();
    assert_eq!(
        block_on(transport.serve_websocket_connection(
            &mut reader,
            &mut writer,
            &mut connection,
            [5; 16],
            &mut clock,
            &mut cancellation,
            &mut application,
        )),
        Err(HttpIoError::Write)
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([4; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
}

#[test]
fn websocket_connection_driver_closes_after_a_peer_close() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let input = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        token(&created)
    );
    let mut bytes = input.into_bytes();
    bytes.extend(masked(true, 8, b""));
    let mut reader = Cursor::new(bytes);
    let mut writer = RecordingWriter::default();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = std::future::pending::<()>();
    block_on(transport.serve_websocket_connection(
        &mut reader,
        &mut writer,
        &mut connection,
        [5; 16],
        &mut clock,
        &mut cancellation,
        &mut application,
    ))
    .unwrap();
    assert_eq!(writer.closes, 1);
}
fn subscribe() -> Vec<u8> {
    Envelope {
        request: Some([3; 16]),
        watch: None,
        message: Message::Subscribe {
            resource: [4; 16],
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "dark".into(),
                supported_kinds: vec![],
            },
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap()
}
fn cancel() -> Vec<u8> {
    cancel_request([7; 16], [8; 16])
}
fn cancel_request(request: [u8; 16], target: [u8; 16]) -> Vec<u8> {
    Envelope {
        request: Some(request),
        watch: None,
        message: Message::Cancel {
            target_kind: TargetKind::Request,
            target,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap()
}
fn resync() -> Vec<u8> {
    Envelope {
        request: Some([9; 16]),
        watch: Some([10; 16]),
        message: Message::Resync,
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap()
}

fn unsubscribe() -> Vec<u8> {
    Envelope {
        request: Some([12; 16]),
        watch: Some([11; 16]),
        message: Message::Unsubscribe,
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap()
}

fn eval(session: [u8; 16], request: [u8; 16], source: &str) -> Vec<u8> {
    let mut envelope = Envelope {
        request: Some(request),
        watch: None,
        message: Message::Eval {
            source: source.into(),
            database: DatabaseContext {
                database: [2; 16],
                snapshot: None,
            },
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "terminal/dark".into(),
                supported_kinds: vec![],
            },
            fingerprint: [0; 32],
        },
        extensions: BTreeMap::new(),
    };
    let fingerprint =
        canonical_request_fingerprint(session, &envelope, Limits::default().protocol).unwrap();
    if let Message::Eval {
        fingerprint: sent, ..
    } = &mut envelope.message
    {
        *sent = fingerprint;
    }
    envelope.encode(Limits::default().protocol).unwrap()
}

fn create(host: &mut LiveHost, issuer: &mut Issuer) -> SessionCredential {
    block_on(host.create(
        CreateRequest {
            id: [1; 16],
            origin: origin(),
            expires_at: 100,
            now: 0,
            subscribe: &subscribe(),
        },
        issuer,
    ))
    .unwrap()
}

#[test]
fn http_create_and_resume_negotiate_and_replace_connections() {
    assert_eq!(
        LiveHost::negotiate_subprotocol(&["other", SUBPROTOCOL]),
        Ok(SUBPROTOCOL)
    );
    assert_eq!(
        LiveHost::negotiate_subprotocol(&["other"]),
        Err(Error::UnsupportedSubprotocol)
    );
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [5; 16],
            now: 1
        }))
        .unwrap(),
        orna_security_v1::AttachOutcome::Attached
    );
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [6; 16],
            now: 2
        }))
        .unwrap(),
        orna_security_v1::AttachOutcome::Replaced(AttachmentId::new([5; 16]))
    );
    assert_eq!(
        block_on(host.handle_frame([5; 16], 3, Frame::Close)),
        Err(Error::Closed)
    );
    assert_eq!(
        block_on(host.handle_frame([6; 16], 3, Frame::Close)),
        Ok(FrameOutcome::Closed)
    );
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [7; 16],
            now: 4,
        }))
        .unwrap(),
        orna_security_v1::AttachOutcome::Reconnected
    );
}

#[test]
fn http_contract_has_stable_status_headers_and_redacted_errors() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let payload = subscribe();
    let create = block_on(host.http_create(
        CreateRequest {
            id: [1; 16],
            origin: origin(),
            expires_at: 100,
            now: 0,
            subscribe: &payload,
        },
        &mut issuer,
    ));
    assert_eq!(create.status, 201);
    assert_eq!(
        create.headers,
        vec![("content-type", "application/orna-live-v1")]
    );
    let HttpBody::Session(credential) = create.body else {
        panic!("session body expected");
    };
    let resume = block_on(host.http_resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }));
    assert_eq!(resume.status, 101);
    assert_eq!(
        resume.headers,
        vec![
            ("upgrade", "websocket"),
            ("sec-websocket-protocol", SUBPROTOCOL),
        ]
    );
    let deleted = block_on(host.http_delete(DeleteRequest { id: [1; 16] }, &mut Delete(true)));
    assert_eq!(deleted.status, 204);
    assert_eq!(deleted.body, HttpBody::Empty);
}

#[test]
fn frames_are_bounded_binary_canonical_and_cancellable() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    host.reserve_request([1; 16], [8; 16]).unwrap();
    host.start_request([1; 16], [8; 16]).unwrap();
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.handle_frame([5; 16], 2, Frame::Text("x".into()))),
        Err(Error::InvalidFrame)
    );
    assert_eq!(
        block_on(host.handle_frame([5; 16], 2, Frame::Binary(vec![0xff]))),
        Err(Error::InvalidMessage)
    );
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(cancel()), &mut application,))
            .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Cancelled)
    );
    assert_eq!(
        block_on(host.handle_frame([5; 16], 2, Frame::Binary(resync()))),
        Err(Error::Denied)
    );
    assert_eq!(application.calls, 1);
}

#[test]
fn rejected_non_durable_cancellation_callback_does_not_cancel_the_target() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    host.reserve_request([1; 16], [8; 16]).unwrap();
    host.start_request([1; 16], [8; 16]).unwrap();
    let mut rejecting = UnitApplication {
        reject_cancel: true,
        ..UnitApplication::default()
    };
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(cancel()), &mut rejecting,)),
        Err(Error::ApplicationRejected)
    );
    let mut accepting = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame(
            [5; 16],
            3,
            Frame::Binary(cancel_request([13; 16], [8; 16])),
            &mut accepting,
        ))
        .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Cancelled)
    );
    assert_eq!(accepting.calls, 1);
}

#[test]
fn dispatch_computes_fingerprints_and_replays_terminal_results() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let first = eval([1; 16], [20; 16], "1");
    let replay =
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(first.clone()), &mut application))
            .unwrap();
    assert_eq!(application.calls, 1);
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(first), &mut application,)),
        Ok(replay.clone())
    );
    assert_eq!(application.calls, 1);
    let different_input = eval([1; 16], [20; 16], "2");
    assert_eq!(
        block_on(
            host.dispatch_frame([5; 16], 2, Frame::Binary(different_input), &mut application,)
        ),
        Err(Error::RequestMismatch)
    );
    assert_eq!(application.calls, 1);
}

#[test]
fn rejected_requests_retain_failure_identity_and_do_not_reexecute() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication {
        reject: true,
        ..UnitApplication::default()
    };
    let first = eval([1; 16], [21; 16], "1");
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(first.clone()), &mut application,)),
        Err(Error::ApplicationRejected)
    );
    assert_eq!(application.calls, 1);
    assert_eq!(
        block_on(host.dispatch_frame(
            [5; 16],
            2,
            Frame::Binary(eval([1; 16], [21; 16], "2")),
            &mut application,
        )),
        Err(Error::RequestMismatch)
    );
    let replay =
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(first), &mut application)).unwrap();
    assert!(matches!(
        replay.response.unwrap().message,
        Message::Result {
            status: ResultStatus::Failure,
            value: None,
            ..
        }
    ));
    assert_eq!(application.calls, 1);
}

#[test]
fn durable_runtime_replays_a_terminal_request_after_host_reconstruction() {
    let (root, repository) = durable_repository();
    let mut first_host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut first_host, &mut issuer);
    block_on(first_host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let request = eval([1; 16], [23; 16], "1");
    let mut first_application = UnitApplication::default();
    let first = block_on(first_host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(request.clone()),
        &mut first_application,
    ))
    .unwrap();
    assert_eq!(first_application.calls, 1);
    drop(first_host);

    let mut second_host = durable_host(open_durable_state(&repository));
    let mut second_issuer = Issuer(1, None);
    let second_credential = create(&mut second_host, &mut second_issuer);
    block_on(second_host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &second_credential,
        attachment: [6; 16],
        now: 3,
    }))
    .unwrap();
    let mut second_application = UnitApplication::default();
    assert_eq!(
        block_on(second_host.dispatch_frame(
            [6; 16],
            4,
            Frame::Binary(request),
            &mut second_application,
        )),
        Ok(first)
    );
    assert_eq!(second_application.calls, 0);
    assert_eq!(
        block_on(second_host.dispatch_frame(
            [6; 16],
            4,
            Frame::Binary(eval([1; 16], [23; 16], "2")),
            &mut second_application,
        )),
        Err(Error::RequestMismatch)
    );
    drop(second_host);
    remove_test_repository(&root);
}

#[test]
fn durable_request_status_recovers_states_and_enforces_target_fingerprint() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let states = [
        ([31; 16], [41; 32], orna_protocol_v1::RequestState::Reserved),
        ([32; 16], [42; 32], orna_protocol_v1::RequestState::Running),
        ([33; 16], [43; 32], orna_protocol_v1::RequestState::Terminal),
        ([34; 16], [44; 32], orna_protocol_v1::RequestState::Terminal),
        ([35; 16], [45; 32], orna_protocol_v1::RequestState::Orphaned),
    ];
    for (target, fingerprint, _) in states {
        let identity = RequestIdentity {
            session_id: [1; 16],
            request_id: target,
        };
        block_on(runtime.reserve_request(identity, fingerprint)).unwrap();
        if target != [31; 16] {
            block_on(runtime.start_request(identity, fingerprint)).unwrap();
        }
        let terminal = TerminalOutcome::new(Vec::new()).unwrap();
        match target[0] {
            33 => {
                block_on(runtime.complete_request(identity, fingerprint, terminal)).unwrap();
            }
            34 => {
                block_on(runtime.cancel_request(identity, fingerprint, terminal)).unwrap();
            }
            35 => {
                block_on(runtime.orphan_request(identity, fingerprint, terminal)).unwrap();
            }
            _ => {}
        }
    }
    drop(runtime);

    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [6; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    for (request, (target, fingerprint, state)) in
        [[51; 16], [52; 16], [53; 16], [54; 16], [55; 16]]
            .into_iter()
            .zip(states)
    {
        let status_request = Envelope {
            request: Some(request),
            watch: None,
            message: Message::RequestStatus {
                target,
                fingerprint,
            },
            extensions: BTreeMap::new(),
        }
        .encode(Limits::default().protocol)
        .unwrap();
        let outcome = block_on(host.dispatch_frame(
            [6; 16],
            2,
            Frame::Binary(status_request),
            &mut application,
        ))
        .unwrap();
        assert!(matches!(
            outcome.response.unwrap().message,
            Message::RequestStatusResult {
                target: returned_target,
                state: returned_state,
                fingerprint: Some(returned_fingerprint),
                result: None,
            } if returned_target == target
                && returned_state == state
                && returned_fingerprint == fingerprint
        ));
    }
    let mismatch = Envelope {
        request: Some([61; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [31; 16],
            fingerprint: [0; 32],
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    assert_eq!(
        block_on(host.dispatch_frame([6; 16], 2, Frame::Binary(mismatch), &mut application,)),
        Err(Error::RequestMismatch)
    );
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_reclaims_a_reserved_request_after_host_reconstruction() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [24; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let runtime = open_durable_state(&repository);
    block_on(runtime.reserve_request(
        RequestIdentity {
            session_id: [1; 16],
            request_id: [24; 16],
        },
        fingerprint,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [7; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame([7; 16], 2, Frame::Binary(request), &mut application,))
            .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Accepted)
    );
    assert_eq!(application.calls, 1);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_cancels_a_recovered_running_request_and_replays_cancellation() {
    let (root, repository) = durable_repository();
    let target = eval([1; 16], [25; 16], "1");
    let target_fingerprint = request_fingerprint(&target, [1; 16]);
    let runtime = open_durable_state(&repository);
    let target_identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [25; 16],
    };
    block_on(runtime.reserve_request(target_identity, target_fingerprint)).unwrap();
    block_on(runtime.start_request(target_identity, target_fingerprint)).unwrap();
    drop(runtime);

    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [8; 16],
        now: 1,
    }))
    .unwrap();
    let cancel = Envelope {
        request: Some([26; 16]),
        watch: None,
        message: Message::Cancel {
            target_kind: TargetKind::Request,
            target: [25; 16],
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame([8; 16], 2, Frame::Binary(cancel), &mut application,))
            .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Cancelled)
    );
    assert_eq!(application.calls, 1);
    assert!(matches!(
        block_on(host.dispatch_frame([8; 16], 3, Frame::Binary(target), &mut application,))
            .unwrap()
            .outcome,
        FrameOutcome::Cancelled
    ));
    assert_eq!(application.calls, 1);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_orphans_a_running_eval_after_host_reconstruction() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [25; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [25; 16],
    };
    let runtime = open_durable_state(&repository);
    block_on(runtime.reserve_request(identity, fingerprint)).unwrap();
    block_on(runtime.start_request(identity, fingerprint)).unwrap();
    drop(runtime);

    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [7; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let first =
        block_on(host.dispatch_frame([7; 16], 2, Frame::Binary(request.clone()), &mut application))
            .unwrap();
    assert!(matches!(
        first.response.as_ref().unwrap().message,
        Message::Result {
            status: ResultStatus::RetainedWithoutValue,
            value: None,
            fingerprint: returned_fingerprint,
            diagnostic: None,
        } if returned_fingerprint == fingerprint
    ));
    assert_eq!(application.calls, 0);

    let status = block_on(open_durable_state(&repository).request_status_for_identity(identity))
        .unwrap()
        .unwrap();
    assert_eq!(status.state, orna_runtime_v1::RequestState::Orphaned);
    assert_eq!(status.fingerprint, fingerprint);

    let status_request = Envelope {
        request: Some([26; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [25; 16],
            fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let status_outcome =
        block_on(host.dispatch_frame([7; 16], 3, Frame::Binary(status_request), &mut application))
            .unwrap();
    assert!(matches!(
        status_outcome.response.unwrap().message,
        Message::RequestStatusResult {
            target: returned_target,
            state: orna_protocol_v1::RequestState::Orphaned,
            fingerprint: Some(returned_fingerprint),
            result: Some(_),
        } if returned_target == [25; 16] && returned_fingerprint == fingerprint
    ));

    assert_eq!(
        block_on(host.dispatch_frame([7; 16], 4, Frame::Binary(request), &mut application,)),
        Ok(first)
    );
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_rejects_a_retained_response_with_the_wrong_message_shape() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [27; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let retained = Envelope {
        request: Some([27; 16]),
        watch: None,
        message: Message::RequestStatusResult {
            target: [28; 16],
            state: orna_protocol_v1::RequestState::Unknown,
            fingerprint: None,
            result: None,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let runtime = open_durable_state(&repository);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [27; 16],
    };
    block_on(runtime.reserve_request(identity, fingerprint)).unwrap();
    block_on(runtime.start_request(identity, fingerprint)).unwrap();
    block_on(runtime.complete_request(
        identity,
        fingerprint,
        TerminalOutcome::new(retained).unwrap(),
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [9; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame([9; 16], 2, Frame::Binary(request), &mut application,)),
        Err(Error::RuntimeUnavailable)
    );
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn cancelling_a_terminal_request_is_idempotent_and_preserves_its_result() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let target = eval([1; 16], [20; 16], "1");
    let target_outcome =
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(target.clone()), &mut application))
            .unwrap();
    let cancel = Envelope {
        request: Some([22; 16]),
        watch: None,
        message: Message::Cancel {
            target_kind: TargetKind::Request,
            target: [20; 16],
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(cancel), &mut application,))
            .unwrap()
            .outcome,
        FrameOutcome::Cancelled
    );
    assert_eq!(application.calls, 1);
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(target), &mut application,)),
        Ok(target_outcome)
    );
    assert_eq!(application.calls, 1);
}

#[test]
fn dispatches_request_status_and_rejects_unsupported_client_operations() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    host.reserve_request([1; 16], [8; 16]).unwrap();
    let status = Envelope {
        request: Some([9; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [8; 16],
            fingerprint: [0; 32],
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    assert_eq!(
        block_on(host.handle_frame([5; 16], 2, Frame::Binary(status))),
        Ok(FrameOutcome::Accepted)
    );
    assert_eq!(
        block_on(host.handle_frame([5; 16], 2, Frame::Binary(subscribe()))),
        Err(Error::UnsupportedOperation)
    );
}

#[test]
fn unsubscribe_of_an_absent_watch_is_an_idempotent_unit_success() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let outcome =
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(unsubscribe()), &mut application))
            .unwrap();
    assert_eq!(outcome.outcome, FrameOutcome::Accepted);
    assert!(matches!(
        outcome.response.unwrap().message,
        Message::Result {
            status: ResultStatus::Success,
            value: Some(_),
            ..
        }
    ));
    assert_eq!(application.calls, 0);
}

#[test]
fn request_retention_cannot_be_shorter_than_the_reconnect_lease() {
    let mut limits = TransportLimits::default();
    limits.request_retention_ms -= 1;
    assert!(matches!(
        LiveTransport::new(host(), limits),
        Err(Error::Limit)
    ));
}

#[test]
fn deletion_failure_closes_fail_closed_without_sensitive_diagnostics() {
    let mut host = host();
    let mut issuer = Issuer(0x5a, None);
    let credential = create(&mut host, &mut issuer);
    let rendered = format!("{credential:?} {}", Error::DeletionFailed);
    assert!(!rendered.contains("5a"));
    assert!(!rendered.contains("app.example"));
    assert_eq!(
        block_on(host.delete(DeleteRequest { id: [1; 16] }, &mut Delete(false))),
        Err(Error::DeletionFailed)
    );
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [5; 16],
            now: 1
        })),
        Err(Error::Closed)
    );
}

fn wire(method: &str, path: &str, body: &str) -> WireRequest {
    WireRequest {
        method: method.into(),
        path: path.into(),
        headers: vec![
            ("origin".into(), "https://app.example".into()),
            ("content-type".into(), "application/json".into()),
        ],
        body: body.as_bytes().to_vec(),
    }
}
fn uuid(value: u8) -> String {
    let raw = format!("{value:02x}").repeat(16);
    format!(
        "{}-{}-{}-{}-{}",
        &raw[..8],
        &raw[8..12],
        &raw[12..16],
        &raw[16..20],
        &raw[20..]
    )
}
fn token(response: &orna_live_v1::WireResponse) -> String {
    let body = String::from_utf8(response.body.clone()).unwrap();
    body.split("\"resume_token\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .into()
}
fn masked(fin: bool, opcode: u8, body: &[u8]) -> Vec<u8> {
    assert!(body.len() < 126);
    let key = [1, 2, 3, 4];
    let length = u8::try_from(body.len()).expect("test frame is short");
    let mut frame = vec![(if fin { 128 } else { 0 }) | opcode, 128 | length];
    frame.extend(key);
    frame.extend(
        body.iter()
            .enumerate()
            .map(|(index, byte)| byte ^ key[index % 4]),
    );
    frame
}

#[test]
#[allow(clippy::too_many_lines)]
fn live_http_routes_are_exact_origin_checked_and_rotate_scoped_tokens() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let rejected = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            r#"{"database":"bad","protocol":"orna.present.v1"}"#,
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(rejected.status, 400);
    let mut missing_type = wire(
        "POST",
        "/orna/session",
        &format!(
            r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
            uuid(2)
        ),
    );
    missing_type
        .headers
        .retain(|(name, _)| name != "content-type");
    assert_eq!(
        block_on(transport.handle(missing_type, 0, &mut authority, &mut issuer, &mut deletion,))
            .status,
        400
    );
    let rejected = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2),
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(rejected.status, 400);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(created.status, 201, "{created:?}");
    assert!(
        String::from_utf8(created.body.clone())
            .unwrap()
            .contains("\"websocket_path\":\"/orna/live/01010101-0101-0101-0101-010101010101\"")
    );
    assert!(created.headers[1].1.contains(
        "Path=/orna/live/01010101-0101-0101-0101-010101010101; HttpOnly; SameSite=Strict; Secure"
    ));
    let first = token(&created);
    let resumed = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session/01010101-0101-0101-0101-010101010101/resume",
            &format!(r#"{{"resume_token":"{first}","protocol":"orna.present.v1"}}"#),
        ),
        1,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(resumed.status, 200);
    let second = token(&resumed);
    assert_ne!(first, second);
    let mut upgrade = wire("GET", "/orna/live/01010101-0101-0101-0101-010101010101", "");
    upgrade.headers.extend([
        ("connection".into(), "Upgrade".into()),
        ("upgrade".into(), "websocket".into()),
        ("sec-websocket-version".into(), "13".into()),
        (
            "sec-websocket-key".into(),
            "dGhlIHNhbXBsZSBub25jZQ==".into(),
        ),
        ("sec-websocket-protocol".into(), SUBPROTOCOL.into()),
        ("cookie".into(), format!("orna_session={second}")),
    ]);
    assert_eq!(block_on(transport.upgrade(upgrade, [5; 16], 2)).status, 101);
    let replay = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session/01010101-0101-0101-0101-010101010101/resume",
            &format!(r#"{{"resume_token":"{first}","protocol":"orna.present.v1"}}"#),
        ),
        2,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(replay.status, 410);
    let mut deleted = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    deleted
        .headers
        .push(("authorization".into(), format!("Bearer {second}")));
    assert_eq!(
        block_on(transport.handle(deleted, 2, &mut authority, &mut issuer, &mut deletion)).status,
        204
    );
}

#[test]
fn websocket_upgrade_fragmentation_and_controls_are_checked_and_forwarded() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let cookie = token(&created);
    let mut upgrade = wire("GET", "/orna/live/01010101-0101-0101-0101-010101010101", "");
    upgrade.headers.extend([
        ("connection".into(), "Upgrade".into()),
        ("upgrade".into(), "websocket".into()),
        ("sec-websocket-version".into(), "13".into()),
        (
            "sec-websocket-key".into(),
            "dGhlIHNhbXBsZSBub25jZQ==".into(),
        ),
        (
            "sec-websocket-protocol".into(),
            format!("other, {SUBPROTOCOL}"),
        ),
        ("cookie".into(), format!("orna_session={cookie}")),
    ]);
    let upgraded = block_on(transport.upgrade(upgrade.clone(), [5; 16], 1));
    assert_eq!(upgraded.status, 101);
    assert!(
        upgraded
            .headers
            .iter()
            .any(|(name, value)| name == "sec-websocket-accept"
                && value == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
    );
    let mut no_protocol = upgrade;
    no_protocol
        .headers
        .retain(|(name, _)| name != "sec-websocket-protocol");
    assert_eq!(
        block_on(transport.upgrade(no_protocol, [6; 16], 1)).status,
        400
    );
    let message = resync();
    let split = message.len() / 2;
    let mut socket = WebSocketState::new([5; 16]);
    assert!(
        block_on(transport.receive(&mut socket, 2, &masked(false, 2, &message[..split])))
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &masked(true, 0, &message[split..]))),
        Err(Error::Denied)
    );
    let pong = block_on(transport.receive(&mut socket, 2, &masked(true, 9, b"p"))).unwrap();
    assert_eq!(pong, vec![WebSocketOutput::Pong(b"p".to_vec())]);
    assert_eq!(
        encode_websocket_output(&pong[0], TransportLimits::default()).unwrap(),
        Some(vec![0x8a, 1, b'p'])
    );
    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &masked(true, 1, b"text"))),
        Err(Error::InvalidFrame)
    );
    let close = block_on(transport.receive(
        &mut socket,
        2,
        &[masked(true, 8, b""), masked(true, 9, b"ignored ping")].concat(),
    ))
    .unwrap();
    assert_eq!(
        close,
        vec![
            WebSocketOutput::Accepted(FrameOutcome::Closed),
            WebSocketOutput::Close
        ]
    );
    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &masked(true, 2, &message))),
        Err(Error::Closed)
    );
}

#[test]
fn websocket_output_encoder_emits_minimal_unmasked_frames() {
    for (size, header) in [(0, vec![0x82, 0]), (125, vec![0x82, 125])] {
        let output = WebSocketOutput::Binary {
            outcome: FrameOutcome::Accepted,
            payload: vec![7; size],
        };
        let encoded = encode_websocket_output(&output, TransportLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!(&encoded[..header.len()], header.as_slice());
        assert_eq!(encoded.len(), header.len() + size);
    }
    let cases = [
        (126, vec![0x82, 126, 0, 126]),
        (u16::MAX as usize, vec![0x82, 126, 255, 255]),
        (
            u16::MAX as usize + 1,
            vec![0x82, 127, 0, 0, 0, 0, 0, 1, 0, 0],
        ),
    ];
    for (size, header) in cases {
        let output = WebSocketOutput::Binary {
            outcome: FrameOutcome::Accepted,
            payload: vec![7; size],
        };
        let encoded = encode_websocket_output(&output, TransportLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!(&encoded[..header.len()], header.as_slice());
        assert_eq!(encoded.len(), header.len() + size);
    }
    assert_eq!(
        encode_websocket_output(
            &WebSocketOutput::Pong(vec![1, 2]),
            TransportLimits::default()
        )
        .unwrap(),
        Some(vec![0x8a, 2, 1, 2])
    );
    assert_eq!(
        encode_websocket_output(&WebSocketOutput::Close, TransportLimits::default()).unwrap(),
        Some(vec![0x88, 0])
    );
    assert_eq!(
        encode_websocket_output(
            &WebSocketOutput::Accepted(FrameOutcome::Accepted),
            TransportLimits::default()
        )
        .unwrap(),
        None
    );
}

#[test]
fn websocket_output_encoder_rejects_oversized_payloads_before_encoding() {
    let limits = TransportLimits {
        max_frame_bytes: 2,
        max_outgoing_bytes: 2,
        ..TransportLimits::default()
    };
    assert_eq!(
        encode_websocket_output(
            &WebSocketOutput::Binary {
                outcome: FrameOutcome::Accepted,
                payload: vec![7; 3],
            },
            limits
        ),
        Err(orna_live_v1::WebSocketEncodeError::Limit)
    );
    let control_limits = TransportLimits {
        max_frame_bytes: 256,
        max_outgoing_bytes: 256,
        ..TransportLimits::default()
    };
    assert_eq!(
        encode_websocket_output(&WebSocketOutput::Pong(vec![7; 126]), control_limits),
        Err(orna_live_v1::WebSocketEncodeError::Limit)
    );
}

#[test]
fn websocket_close_payloads_require_valid_codes_and_utf8_reasons() {
    for payload in [
        vec![0x03],
        vec![0x03, 0xed],
        vec![0x03, 0xec],
        vec![0x03, 0xe8, 0xff],
    ] {
        let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
        let mut socket = WebSocketState::new([5; 16]);
        assert_eq!(
            block_on(transport.receive(&mut socket, 2, &masked(true, 8, &payload))),
            Err(Error::InvalidFrame)
        );
    }
}

#[test]
fn application_responses_reach_the_websocket_as_canonical_binary() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let cookie = token(&created);
    let mut upgrade = wire("GET", "/orna/live/01010101-0101-0101-0101-010101010101", "");
    upgrade.headers.extend([
        ("connection".into(), "Upgrade".into()),
        ("upgrade".into(), "websocket".into()),
        ("sec-websocket-version".into(), "13".into()),
        (
            "sec-websocket-key".into(),
            "dGhlIHNhbXBsZSBub25jZQ==".into(),
        ),
        ("sec-websocket-protocol".into(), SUBPROTOCOL.into()),
        ("cookie".into(), format!("orna_session={cookie}")),
    ]);
    assert_eq!(block_on(transport.upgrade(upgrade, [5; 16], 1)).status, 101);

    let mut socket = WebSocketState::new([5; 16]);
    let mut application = UnitApplication::default();
    let output = block_on(transport.receive_with_application(
        &mut socket,
        2,
        &masked(true, 2, &unsubscribe()),
        &mut application,
    ))
    .unwrap();
    assert_eq!(output.len(), 1);
    let WebSocketOutput::Binary { outcome, payload } = &output[0] else {
        panic!("application response must be a binary WebSocket output");
    };
    assert_eq!(*outcome, FrameOutcome::Accepted);
    let response = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert!(matches!(response.message, Message::Result { .. }));
}
