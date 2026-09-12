use futures::{
    executor::block_on,
    io::{AsyncRead, AsyncWrite, Cursor},
};
use orna_foundation_v1::CanonicalValue;
use orna_live_v1::{
    CreateRequest, DeleteRequest, Error, Frame, FrameOutcome, HttpBody, HttpConnection,
    HttpConnectionError, HttpEncodeError, HttpIoError, HttpParseError, Limits, ListenerBindError,
    ListenerExposure, LiveApplication, LiveCredentialIssuer, LiveHost, LiveListenerAcceptor,
    LiveSessionAuthority, LiveSessionChildren, LiveTransport, ResumeRequest, SUBPROTOCOL,
    SessionCredential, SessionMetadata, TransportLimits, WebSocketOutput, WebSocketState,
    WireRequest, WireResponse, encode_websocket_output, parse_http_request,
};
use orna_protocol_v1::{
    DatabaseContext, Envelope, Message, PresentationContext, ResultBody, ResultStatus, TargetKind,
    canonical_request_fingerprint,
};
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    FaultInjector, FaultPoint, RequestIdentity, RequestOwner, RequestState, RunObservationStatus,
    RuntimeError, RuntimeIdentity, RuntimeState, TableMutation, TerminalOutcome,
};
use orna_security_v1::{
    AttachmentId, BoundaryError, CredentialIssuer, Origin, OriginPolicy, SessionBoundary,
    SessionDeletionAdapter,
};
use orna_serving_v1::{Credential as ServingCredential, Limits as ServingLimits, Serving};
use std::{
    collections::BTreeMap,
    fs,
    future::Future,
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
    sync::mpsc,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

struct FailFirstWriter {
    writes: usize,
}

struct FailAfterFirstWrite {
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

struct PrefixThenPendingReader {
    prefix: Cursor<Vec<u8>>,
}

impl AsyncRead for PrefixThenPendingReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        bytes: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match std::pin::Pin::new(&mut self.prefix).poll_read(context, bytes) {
            std::task::Poll::Ready(Ok(0)) => std::task::Poll::Pending,
            result => result,
        }
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

#[derive(Default)]
struct PendingListenerAcceptor {
    accepts: usize,
}

impl LiveListenerAcceptor for PendingListenerAcceptor {
    type Accept<'a> = std::future::Pending<std::io::Result<TcpStream>>;

    fn accept<'a>(&'a mut self, _: &'a TcpListener) -> Self::Accept<'a> {
        self.accepts += 1;
        std::future::pending()
    }
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

impl AsyncWrite for FailAfterFirstWrite {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.writes += 1;
        std::task::Poll::Ready(if self.writes == 1 {
            Ok(bytes.len())
        } else {
            Err(std::io::Error::other("write rejected"))
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
struct RecordingDelete {
    calls: usize,
}

impl SessionDeletionAdapter for RecordingDelete {
    type Error = ();

    fn delete(&mut self, _: orna_security_v1::SessionId) -> Result<(), Self::Error> {
        self.calls += 1;
        Ok(())
    }
}

#[derive(Default)]
struct RecordingChildren {
    calls: usize,
    requests: Vec<RequestIdentity>,
    fail: bool,
}

impl LiveSessionChildren for RecordingChildren {
    fn cancel_and_join_session<'a>(
        &'a mut self,
        _: [u8; 16],
        requests: &'a [RequestIdentity],
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>> {
        self.calls += 1;
        self.requests = requests.to_vec();
        Box::pin(async move {
            if self.fail {
                Err(Error::ApplicationRejected)
            } else {
                Ok(())
            }
        })
    }
}

struct LateAdmissionChildren<'a> {
    runtime: &'a RuntimeState,
    identity: RequestIdentity,
    fingerprint: [u8; 32],
    calls: usize,
}

impl LiveSessionChildren for LateAdmissionChildren<'_> {
    fn cancel_and_join_session<'a>(
        &'a mut self,
        _: [u8; 16],
        _: &'a [RequestIdentity],
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>> {
        self.calls += 1;
        Box::pin(async move {
            match self
                .runtime
                .reserve_request(self.identity, self.fingerprint)
                .await
            {
                Err(RuntimeError::SessionClosed) => Ok(()),
                Ok(_) => Err(Error::ApplicationRejected),
                Err(_) => Err(Error::RuntimeUnavailable),
            }
        })
    }
}

#[derive(Clone, Copy, Default)]
enum UnitEvalOutcome {
    #[default]
    Unit,
    SemanticFailure,
}

#[derive(Default)]
struct UnitApplication {
    calls: usize,
    reject: bool,
    reject_cancel: bool,
    eval_outcome: UnitEvalOutcome,
}

struct CompetingTerminalApplication {
    repository: Repository,
    owner: [u8; 16],
    calls: usize,
}

struct FailAt(FaultPoint);

impl FaultInjector for FailAt {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
        if point == self.0 {
            Err(RuntimeError::FaultInjected(point))
        } else {
            Ok(())
        }
    }
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

fn semantic_failure_result(request: [u8; 16], fingerprint: [u8; 32]) -> Envelope {
    Envelope {
        request: Some(request),
        watch: None,
        message: Message::Result {
            status: ResultStatus::Failure,
            value: None,
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
        Ok(match self.eval_outcome {
            UnitEvalOutcome::Unit => unit_result(request, *fingerprint),
            UnitEvalOutcome::SemanticFailure => semantic_failure_result(request, *fingerprint),
        })
    }

    fn watch(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope, Error> {
        Err(Error::UnsupportedOperation)
    }

    fn event(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> Result<Envelope, Error> {
        let Message::Event { fingerprint, .. } = message else {
            return Err(Error::ApplicationRejected);
        };
        self.calls += 1;
        Ok(unit_result(request, *fingerprint))
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

impl LiveApplication for CompetingTerminalApplication {
    fn eval(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> Result<Envelope, Error> {
        let Message::Eval { fingerprint, .. } = message else {
            return Err(Error::ApplicationRejected);
        };
        self.calls += 1;
        let malformed_winner = Envelope {
            request: Some([99; 16]),
            watch: None,
            message: Message::Result {
                status: ResultStatus::Success,
                value: Some(CanonicalValue::unit()),
                fingerprint: *fingerprint,
                diagnostic: None,
            },
            extensions: BTreeMap::new(),
        };
        let repository = self.repository.clone();
        let owner = self.owner;
        let fingerprint = *fingerprint;
        std::thread::spawn(move || {
            let runtime = open_durable_state(&repository);
            let writer = block_on(runtime.acquire_lease(owner)).unwrap();
            block_on(
                runtime.complete_observed_request_with_owner(
                    RequestIdentity {
                        session_id: session,
                        request_id: request,
                    },
                    fingerprint,
                    writer,
                    TerminalOutcome::new(
                        malformed_winner.encode(Limits::default().protocol).unwrap(),
                    )
                    .unwrap(),
                ),
            )
            .unwrap();
        })
        .join()
        .unwrap();
        Ok(unit_result(request, fingerprint))
    }

    fn watch(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope, Error> {
        Err(Error::UnsupportedOperation)
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

fn durable_host_with_owner(runtime: RuntimeState, owner: [u8; 16]) -> LiveHost {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    LiveHost::with_runtime_state_and_owner(
        Limits::default(),
        boundary,
        Serving::new(ServingLimits::default()).unwrap(),
        runtime,
        owner,
    )
    .unwrap()
}

fn durable_host_after_takeover(
    runtime: RuntimeState,
    owner: [u8; 16],
    lost_owner: RequestOwner,
) -> LiveHost {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    LiveHost::with_runtime_state_after_takeover(
        Limits::default(),
        boundary,
        Serving::new(ServingLimits::default()).unwrap(),
        runtime,
        owner,
        lost_owner,
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
    let listener = LiveTransport::bind_default_listener(0).unwrap();
    let status = listener.status();
    assert_eq!(status.exposure, ListenerExposure::Loopback);
    assert!(status.address.ip().is_loopback());
    let address = status.address;
    let server = thread::spawn(move || {
        let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut authority = Authority;
        let mut issuer = Issuer(7, None);
        let mut deletion = Delete(true);
        transport.serve_one_http_listener(
            listener.listener(),
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
fn listener_policy_reports_loopback_rejects_exposure_and_releases_on_drop() {
    let listener = LiveTransport::bind_default_listener(0).unwrap();
    let address = listener.status().address;
    assert_eq!(listener.status().exposure, ListenerExposure::Loopback);
    assert!(address.ip().is_loopback());

    assert!(matches!(
        LiveTransport::bind_explicit_listener(([0, 0, 0, 0], address.port()).into()),
        Err(ListenerBindError::NonLoopback)
    ));

    drop(listener);
    let _released = TcpListener::bind(address).unwrap();

    let listener = LiveTransport::bind_explicit_listener(([127, 0, 0, 1], 0).into()).unwrap();
    assert_eq!(listener.status().exposure, ListenerExposure::Explicit);
    assert!(listener.status().address.ip().is_loopback());
}

#[test]
fn cancellable_listener_accept_preserves_listener_ownership() {
    let listener = LiveTransport::bind_default_listener(0).unwrap();
    let address = listener.status().address;
    let transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut acceptor = PendingListenerAcceptor::default();
    let mut cancellation = CancelAfterPolls {
        polls: 0,
        ready_after: 2,
    };

    assert!(matches!(
        block_on(transport.accept_listener_with_cancellation(
            &listener,
            &mut acceptor,
            &mut cancellation,
        )),
        Err(HttpIoError::Cancelled)
    ));
    assert_eq!(acceptor.accepts, 1);
    assert_eq!(listener.listener().local_addr().unwrap(), address);
    assert!(TcpListener::bind(address).is_err());
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
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut application = UnitApplication::default();
        transport.serve_one_websocket_listener(
            &listener,
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
fn accepted_tcp_socket_rejects_an_unmasked_client_frame_with_protocol_close() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let (outcome_sender, outcome_receiver) = mpsc::channel();
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
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut application = UnitApplication::default();
        let result = transport.serve_one_websocket_listener(
            &listener,
            &mut connection,
            [5; 16],
            &mut || 1,
            &mut application,
        );
        outcome_sender.send((result, application.calls)).unwrap();
    });

    let request = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        receiver.recv().unwrap()
    );
    let mut client = TcpStream::connect(address).unwrap();
    client.write_all(request.as_bytes()).unwrap();
    client.write_all(&unmasked(true, 2, b"")).unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).unwrap();

    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("serialized handshake response");
    assert!(response.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
    assert_eq!(&response[header_end + 4..], b"\x88\x02\x03\xea");
    let (result, application_calls) = outcome_receiver.recv().unwrap();
    assert_eq!(result, Ok(()));
    assert_eq!(application_calls, 0);
    server.join().unwrap();
}

#[test]
fn accepted_websocket_eof_disconnects_its_attachment() {
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
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut application = UnitApplication::default();
        transport
            .serve_one_websocket_listener(
                &listener,
                &mut connection,
                [5; 16],
                &mut || 1,
                &mut application,
            )
            .unwrap();
        block_on(transport.receive(
            &mut WebSocketState::new([5; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        ))
    });

    let request = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        receiver.recv().unwrap()
    );
    let mut client = TcpStream::connect(address).unwrap();
    client.write_all(request.as_bytes()).unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).unwrap();

    assert!(response.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
    assert_eq!(server.join().unwrap(), Err(Error::Closed));
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
fn websocket_connection_driver_emits_exact_canonical_result_envelope() {
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
    bytes.extend(masked(true, 2, &eval([1; 16], [12; 16], "1")));
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
    assert_eq!(application.calls, 1);
    let output = writer.bytes;
    let header_end = output
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("serialized handshake response");
    assert_eq!(
        &output[header_end + 4..],
        [
            0x82, 0x47, 0xa5, 0x00, 0x01, 0x01, 0x12, 0x02, 0x50, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c,
            0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x03, 0xf6, 0x04,
            0xa4, 0x00, 0x00, 0x01, 0xd9, 0xea, 0x6e, 0x80, 0x02, 0x58, 0x20, 0x1b, 0xb0, 0xd4,
            0x0c, 0x76, 0x63, 0x36, 0x3d, 0xb5, 0x3a, 0x62, 0xce, 0xc1, 0x03, 0x2c, 0x9f, 0x1d,
            0x86, 0xc4, 0x26, 0x70, 0x08, 0xc1, 0x3b, 0x8c, 0xb6, 0x46, 0x0d, 0x0e, 0xde, 0x23,
            0x35, 0x03, 0xf6,
        ]
    );
}

#[test]
fn websocket_connection_driver_cancellation_after_delivery_commits_then_cleans_attachment() {
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
    let credential = token(&created);
    assert_eq!(
        block_on(transport.upgrade(websocket_upgrade(1, &credential), [4; 16], 1)).status,
        101
    );
    let input = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        credential
    );
    let mut reader = PrefixThenPendingReader {
        prefix: Cursor::new(input.into_bytes()),
    };
    let mut writer = Cursor::new(Vec::new());
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = CancelAfterPolls {
        polls: 0,
        ready_after: 4,
    };
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
        Err(HttpIoError::Cancelled)
    );
    assert_eq!(
        block_on(transport.receive(
            &mut WebSocketState::new([5; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        )),
        Err(Error::Closed)
    );
    assert_eq!(
        block_on(transport.receive(
            &mut WebSocketState::new([4; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        )),
        Err(Error::Closed)
    );
    assert!(
        writer
            .into_inner()
            .starts_with(b"HTTP/1.1 101 Switching Protocols\r\n")
    );
    assert_eq!(transport.take_retired_attachments(), vec![[4; 16]]);
}

#[test]
fn websocket_connection_driver_cancellation_during_output_disconnects_its_attachment() {
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
    bytes.extend(masked(true, 9, b"p"));
    let mut reader = Cursor::new(bytes);
    let mut writer = Cursor::new(Vec::new());
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = CancelAfterPolls {
        polls: 0,
        ready_after: 4,
    };
    assert_eq!(
        block_on(transport.serve_websocket_connection(
            &mut reader,
            &mut writer,
            &mut connection,
            [6; 16],
            &mut clock,
            &mut cancellation,
            &mut application,
        )),
        Err(HttpIoError::Cancelled)
    );
    assert_eq!(
        block_on(transport.receive(
            &mut WebSocketState::new([6; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        )),
        Err(Error::Closed)
    );
}

#[test]
fn websocket_connection_driver_output_write_failure_disconnects_its_attachment() {
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
    let mut input = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        token(&created)
    )
    .into_bytes();
    input.extend(masked(true, 9, b"p"));
    let mut reader = Cursor::new(input);
    let mut writer = FailAfterFirstWrite { writes: 0 };
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = std::future::pending::<()>();
    assert_eq!(
        block_on(transport.serve_websocket_connection(
            &mut reader,
            &mut writer,
            &mut connection,
            [7; 16],
            &mut clock,
            &mut cancellation,
            &mut application,
        )),
        Err(HttpIoError::Write)
    );
    assert_eq!(
        block_on(transport.receive(
            &mut WebSocketState::new([7; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        )),
        Err(Error::Closed)
    );
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
    assert_eq!(
        block_on(transport.upgrade(request.clone(), [4; 16], 1)).status,
        101
    );
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
    // The failed 101 write aborts only the candidate handoff. The active
    // attachment remains usable above; the candidate stays fenced until the
    // executable owner cancels and joins it.
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
    assert_eq!(
        transport
            .begin_websocket_upgrade(&request, [5; 16], 2)
            .unwrap_err()
            .status,
        503
    );
    assert!(transport.acknowledge_retired_attachment([5; 16]));
    assert_eq!(block_on(transport.upgrade(request, [5; 16], 2)).status, 101);
    assert_eq!(transport.take_retired_attachments(), vec![[4; 16]]);
}

#[test]
fn websocket_connection_driver_cancellation_retires_the_stalled_candidate_before_failure() {
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
    let credential = token(&created);
    let request = websocket_upgrade(1, &credential);
    assert_eq!(
        block_on(transport.upgrade(request.clone(), [4; 16], 1)).status,
        101
    );

    let mut reader = Cursor::new(
        format!(
            "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
            uuid(1), SUBPROTOCOL, credential
        )
        .into_bytes(),
    );
    let mut writer = PendingWriter;
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 2;
    let mut cancellation = CancelAfterPolls {
        polls: 0,
        ready_after: 3,
    };

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
        Err(HttpIoError::Cancelled)
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);

    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([4; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
    assert_eq!(block_on(transport.upgrade(request, [6; 16], 2)).status, 101);
    assert_eq!(transport.take_retired_attachments(), vec![[4; 16]]);
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

fn event(session: [u8; 16], request: [u8; 16], watch: [u8; 16]) -> Vec<u8> {
    let mut envelope = Envelope {
        request: Some(request),
        watch: Some(watch),
        message: Message::Event {
            revision: 0,
            action: [13; 16],
            value: CanonicalValue::unit(),
            fingerprint: [0; 32],
        },
        extensions: BTreeMap::new(),
    };
    let fingerprint =
        canonical_request_fingerprint(session, &envelope, Limits::default().protocol).unwrap();
    if let Message::Event {
        fingerprint: sent, ..
    } = &mut envelope.message
    {
        *sent = fingerprint;
    }
    envelope.encode(Limits::default().protocol).unwrap()
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
fn rejected_cross_layer_reconnect_preserves_a_valid_session() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    let mismatched = SessionCredential {
        security: credential.security.clone(),
        serving: ServingCredential::new([9; 32]),
    };

    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &mismatched,
            attachment: [5; 16],
            now: 1,
        })),
        Err(Error::Denied)
    );
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [6; 16],
            now: 1,
        })),
        Ok(orna_security_v1::AttachOutcome::Attached)
    );
}

#[test]
fn http_resume_retires_the_active_attachment_without_closing_the_session() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [5; 16],
            now: 1,
        }))
        .unwrap(),
        orna_security_v1::AttachOutcome::Attached
    );

    let mut replacement_issuer = Issuer(2, None);
    let (replacement, retired) = block_on(host.rotate_and_retire(
        [1; 16],
        &origin(),
        &credential,
        2,
        &mut replacement_issuer,
    ))
    .unwrap();
    assert_eq!(retired, Some([5; 16]));
    assert_eq!(
        block_on(host.handle_frame([5; 16], 3, Frame::Close)),
        Err(Error::Closed)
    );
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &replacement,
            attachment: [6; 16],
            now: 3,
        }))
        .unwrap(),
        orna_security_v1::AttachOutcome::Reconnected
    );
    assert_eq!(
        block_on(host.handle_frame([6; 16], 4, Frame::Close)),
        Ok(FrameOutcome::Closed)
    );
}

#[test]
fn websocket_replacement_queues_retirement_and_close_is_idempotent() {
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
    let upgrade = |attachment| {
        let mut request = wire("GET", "/orna/live/01010101-0101-0101-0101-010101010101", "");
        request.headers.extend([
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
        (request, attachment)
    };
    assert_eq!(
        block_on(transport.upgrade(upgrade([5; 16]).0, [5; 16], 1)).status,
        101
    );
    assert_eq!(
        block_on(transport.upgrade(upgrade([6; 16]).0, [6; 16], 2)).status,
        101
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
    assert_eq!(
        block_on(transport.upgrade(upgrade([5; 16]).0, [5; 16], 3)).status,
        503
    );
    assert_eq!(
        block_on(transport.close_attachment([5; 16], 3)),
        Err(Error::Closed)
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([6; 16]),
            3,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
    assert!(transport.acknowledge_retired_attachment([5; 16]));
    assert_eq!(
        block_on(transport.upgrade(upgrade([5; 16]).0, [5; 16], 4)).status,
        101
    );
    assert_eq!(transport.take_retired_attachments(), vec![[6; 16]]);
    assert_eq!(
        block_on(transport.close_attachment([6; 16], 4)),
        Err(Error::Closed)
    );
    assert_eq!(
        block_on(transport.close_attachment([5; 16], 4)),
        Ok(FrameOutcome::Closed)
    );
    assert!(transport.acknowledge_retired_attachment([6; 16]));
    assert_eq!(transport.take_retired_attachments(), Vec::<[u8; 16]>::new());
}

#[test]
fn websocket_upgrade_reservation_blocks_only_its_session_until_commit() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut deletion = Delete(true);
    let first = block_on(transport.handle(
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
    let first_token = token(&first);
    let second = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(3)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let second_token = token(&second);

    let active = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &first_token), [5; 16], 1)
        .unwrap();
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(active, 1))
            .unwrap()
            .status,
        101
    );

    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &first_token), [6; 16], 2)
        .unwrap();
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(1, &first_token), [7; 16], 2)
            .unwrap_err()
            .status,
        503
    );
    assert_eq!(
        block_on(transport.handle(
            wire(
                "POST",
                "/orna/session/01010101-0101-0101-0101-010101010101/resume",
                &format!(r#"{{"resume_token":"{first_token}","protocol":"orna.present.v1"}}"#),
            ),
            2,
            &mut authority,
            &mut issuer,
            &mut deletion,
        ))
        .status,
        503
    );
    assert_eq!(
        block_on(transport.handle(
            wire(
                "POST",
                "/orna/session/02020202-0202-0202-0202-020202020202/resume",
                &format!(r#"{{"resume_token":"{second_token}","protocol":"orna.present.v1"}}"#),
            ),
            2,
            &mut authority,
            &mut issuer,
            &mut deletion,
        ))
        .status,
        200
    );
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(pending, 2))
            .unwrap()
            .status,
        101
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
}

#[test]
fn websocket_upgrade_rejects_attachment_identity_owned_by_another_handoff() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut deletion = Delete(true);
    let first = block_on(transport.handle(
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
    let second = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(3)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let first_token = token(&first);
    let second_token = token(&second);

    let active = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &first_token), [5; 16], 1)
        .unwrap();
    block_on(transport.commit_websocket_upgrade(active, 1)).unwrap();
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(2, &second_token), [5; 16], 2)
            .unwrap_err()
            .status,
        503
    );

    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &first_token), [6; 16], 2)
        .unwrap();
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(2, &second_token), [6; 16], 2)
            .unwrap_err()
            .status,
        503
    );
    assert!(transport.abort_websocket_upgrade(&pending));
    assert_eq!(transport.take_retired_attachments(), vec![[6; 16]]);
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(2, &second_token), [6; 16], 3)
            .unwrap_err()
            .status,
        503
    );
    assert!(transport.acknowledge_retired_attachment([6; 16]));

    assert_eq!(
        block_on(transport.upgrade(websocket_upgrade(2, &second_token), [6; 16], 3)).status,
        101
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([6; 16]),
            3,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([5; 16]),
            3,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
}

#[test]
fn websocket_upgrade_abort_preserves_attachment_and_consumes_reservation() {
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
    let credential = token(&created);

    let mut malformed = websocket_upgrade(1, &credential);
    malformed
        .headers
        .retain(|(name, _)| name != "sec-websocket-protocol");
    assert_eq!(
        transport
            .begin_websocket_upgrade(&malformed, [5; 16], 1)
            .unwrap_err()
            .status,
        400
    );
    let active = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [5; 16], 1)
        .unwrap();
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(active, 1))
            .unwrap()
            .status,
        101
    );

    let aborted = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 2)
        .unwrap();
    assert!(transport.abort_websocket_upgrade(&aborted));
    assert!(!transport.abort_websocket_upgrade(&aborted));
    assert_eq!(transport.take_retired_attachments(), vec![[6; 16]]);
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 3)
            .unwrap_err()
            .status,
        503
    );
    assert!(transport.acknowledge_retired_attachment([6; 16]));
    assert!(!transport.acknowledge_retired_attachment([6; 16]));
    assert_eq!(
        block_on(transport.upgrade(websocket_upgrade(1, &credential), [6; 16], 3)).status,
        101
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
    // The acknowledged candidate identity is reusable by a later committed
    // handoff, but its consumed reservation must remain inert. In particular,
    // a stale terminal replay cannot consume or retire the new attachment.
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(aborted, 4)),
        Err(Error::Closed)
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([6; 16]),
            4,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
    assert_eq!(transport.take_retired_attachments(), Vec::<[u8; 16]>::new());
}

#[test]
fn websocket_upgrade_expiry_queues_and_fences_candidate_without_replacing_attachment() {
    let limits = TransportLimits {
        lease_ms: 10,
        request_retention_ms: 10,
        ..TransportLimits::default()
    };
    let mut transport = LiveTransport::new(host(), limits).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
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
    let credential = token(&created);
    let second = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(3)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let second_token = token(&second);
    let active = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [5; 16], 1)
        .unwrap();
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(active, 1))
            .unwrap()
            .status,
        101
    );

    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 2)
        .unwrap();
    transport.expire_pending_websocket_upgrades(12);
    assert_eq!(transport.take_retired_attachments(), vec![[6; 16]]);
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 12)
            .unwrap_err()
            .status,
        503
    );
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(2, &second_token), [6; 16], 12)
            .unwrap_err()
            .status,
        503
    );
    assert!(transport.acknowledge_retired_attachment([6; 16]));
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(pending, 12)),
        Err(Error::Closed)
    );
    assert_eq!(
        block_on(transport.upgrade(websocket_upgrade(2, &second_token), [6; 16], 13)).status,
        101
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([6; 16]),
            13,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([5; 16]),
            13,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
    // The expired candidate was retired and acknowledged above. This
    // cross-session admission never replaced the original attachment, so it
    // has no additional worker to retire.
    assert_eq!(transport.take_retired_attachments(), Vec::<[u8; 16]>::new());
}

#[test]
fn foreign_upgrade_reservation_cannot_consume_a_local_pending_handshake() {
    let mut first = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut second = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut first_issuer = Issuer(1, None);
    let mut second_issuer = Issuer(1, None);
    let mut first_authority = Authority;
    let mut second_authority = Authority;
    let mut first_deletion = Delete(true);
    let mut second_deletion = Delete(true);
    let body = format!(
        r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
        uuid(2)
    );
    let first_created = block_on(first.handle(
        wire("POST", "/orna/session", &body),
        0,
        &mut first_authority,
        &mut first_issuer,
        &mut first_deletion,
    ));
    let second_created = block_on(second.handle(
        wire("POST", "/orna/session", &body),
        0,
        &mut second_authority,
        &mut second_issuer,
        &mut second_deletion,
    ));
    let first_pending = first
        .begin_websocket_upgrade(&websocket_upgrade(1, &token(&first_created)), [5; 16], 1)
        .unwrap();
    let second_pending = second
        .begin_websocket_upgrade(&websocket_upgrade(1, &token(&second_created)), [5; 16], 1)
        .unwrap();

    assert!(!second.abort_websocket_upgrade(&first_pending));
    assert_eq!(
        block_on(second.commit_websocket_upgrade(first_pending, 1)),
        Err(Error::Closed)
    );
    assert_eq!(
        second
            .begin_websocket_upgrade(&websocket_upgrade(1, &token(&second_created)), [6; 16], 1)
            .unwrap_err()
            .status,
        503
    );
    assert_eq!(
        block_on(second.commit_websocket_upgrade(second_pending, 1))
            .unwrap()
            .status,
        101
    );
}

#[test]
fn foreign_expired_reservation_commit_cannot_expire_local_pending_handshake() {
    let limits = TransportLimits {
        lease_ms: 10,
        request_retention_ms: 10,
        ..TransportLimits::default()
    };
    let mut first = LiveTransport::new(host(), limits).unwrap();
    let mut second = LiveTransport::new(host(), limits).unwrap();
    let mut first_issuer = Issuer(1, None);
    let mut second_issuer = Issuer(1, None);
    let mut first_authority = Authority;
    let mut second_authority = Authority;
    let mut first_deletion = Delete(true);
    let mut second_deletion = Delete(true);
    let body = format!(
        r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
        uuid(2)
    );
    let first_created = block_on(first.handle(
        wire("POST", "/orna/session", &body),
        0,
        &mut first_authority,
        &mut first_issuer,
        &mut first_deletion,
    ));
    let second_created = block_on(second.handle(
        wire("POST", "/orna/session", &body),
        0,
        &mut second_authority,
        &mut second_issuer,
        &mut second_deletion,
    ));
    let first_pending = first
        .begin_websocket_upgrade(&websocket_upgrade(1, &token(&first_created)), [5; 16], 1)
        .unwrap();
    let second_pending = second
        .begin_websocket_upgrade(&websocket_upgrade(1, &token(&second_created)), [5; 16], 1)
        .unwrap();

    // Ownership is checked before expiry cleanup: a foreign actor cannot use
    // an expired-looking reservation to mutate a local actor's admission.
    assert_eq!(
        block_on(second.commit_websocket_upgrade(first_pending, 11)),
        Err(Error::Closed)
    );
    assert!(second.take_retired_attachments().is_empty());

    // The owning actor still performs the normal expiry transition and fences
    // its candidate before that stale reservation can be used again.
    second.expire_pending_websocket_upgrades(11);
    assert_eq!(second.take_retired_attachments(), vec![[5; 16]]);
    assert_eq!(
        block_on(second.commit_websocket_upgrade(second_pending, 11)),
        Err(Error::Closed)
    );
}

#[test]
fn consumed_reservation_replay_cannot_expire_another_pending_handshake() {
    let limits = TransportLimits {
        lease_ms: 10,
        request_retention_ms: 10,
        ..TransportLimits::default()
    };
    let mut transport = LiveTransport::new(host(), limits).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut deletion = Delete(true);
    let first = block_on(transport.handle(
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
    let second = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(3)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let stale = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &token(&first)), [5; 16], 1)
        .unwrap();
    assert!(transport.abort_websocket_upgrade(&stale));
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
    assert!(transport.acknowledge_retired_attachment([5; 16]));

    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(2, &token(&second)), [6; 16], 1)
        .unwrap();

    // The token is stale before the second candidate's delivery window has
    // been considered.  Its replay cannot run the expiry sweep or publish a
    // retirement for that unrelated candidate.
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(stale, 11)),
        Err(Error::Closed)
    );
    assert!(transport.take_retired_attachments().is_empty());

    transport.expire_pending_websocket_upgrades(11);
    assert_eq!(transport.take_retired_attachments(), vec![[6; 16]]);
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(pending, 11)),
        Err(Error::Closed)
    );
}

#[test]
fn child_aware_delete_retires_active_and_pending_websocket_candidates() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let mut children = RecordingChildren::default();
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
    let credential = token(&created);
    let active = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [5; 16], 1)
        .unwrap();
    block_on(transport.commit_websocket_upgrade(active, 1)).unwrap();
    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 2)
        .unwrap();
    let mut request = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    request
        .headers
        .push(("authorization".into(), format!("Bearer {credential}")));
    assert_eq!(
        block_on(transport.handle_with_children(
            request,
            3,
            &mut authority,
            &mut issuer,
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16], [6; 16]]);
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(pending, 3)),
        Err(Error::Closed)
    );
}

#[test]
fn cleanup_acknowledgement_requires_a_delivered_retirement_notice() {
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
    let credential = token(&created);
    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [5; 16], 1)
        .unwrap();
    assert!(transport.abort_websocket_upgrade(&pending));

    assert!(!transport.acknowledge_retired_attachment([5; 16]));
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [5; 16], 3)
            .unwrap_err()
            .status,
        503
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
    assert!(transport.acknowledge_retired_attachment([5; 16]));
}

#[test]
fn child_free_delete_preserves_session_until_socket_cleanup_can_be_joined() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = RecordingDelete::default();
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
    let credential = token(&created);
    let active = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [5; 16], 1)
        .unwrap();
    block_on(transport.commit_websocket_upgrade(active, 1)).unwrap();
    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 2)
        .unwrap();
    let mut request = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    request
        .headers
        .push(("authorization".into(), format!("Bearer {credential}")));

    assert_eq!(
        block_on(transport.handle(request, 3, &mut authority, &mut issuer, &mut deletion,)).status,
        503
    );
    assert_eq!(deletion.calls, 0);
    assert!(transport.take_retired_attachments().is_empty());
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(pending, 3))
            .unwrap()
            .status,
        101
    );
}

#[test]
fn rejected_delete_preserves_pending_websocket_admission() {
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
    let credential = token(&created);
    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 1)
        .unwrap();
    let mut request = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    request.headers[0].1 = "https://other.example".into();
    request
        .headers
        .push(("authorization".into(), format!("Bearer {credential}")));
    assert_eq!(
        block_on(transport.handle(request, 2, &mut authority, &mut issuer, &mut deletion,)).status,
        403
    );
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [7; 16], 2)
            .unwrap_err()
            .status,
        503
    );
    assert!(transport.abort_websocket_upgrade(&pending));
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
    let origin = origin();
    let deleted = block_on(host.http_delete(
        DeleteRequest {
            id: [1; 16],
            origin: &origin,
            credential: &credential,
            now: 1,
        },
        &mut Delete(true),
    ));
    assert_eq!(deleted.status, 204);
    assert_eq!(deleted.body, HttpBody::Empty);
}

#[test]
#[allow(clippy::too_many_lines)]
fn delete_requires_current_unexpired_bearer_and_original_origin() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut authority = Authority;
    let mut issuer = Issuer(1, None);
    let mut deletion = RecordingDelete::default();
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
    assert_eq!(created.status, 201);
    let credential = token(&created);

    let mut wrong_origin = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    wrong_origin.headers[0].1 = "https://other.example".into();
    wrong_origin
        .headers
        .push(("authorization".into(), format!("Bearer {credential}")));
    assert_eq!(
        block_on(transport.handle(wrong_origin, 1, &mut authority, &mut issuer, &mut deletion,))
            .status,
        403
    );
    assert_eq!(deletion.calls, 0);

    let resumed = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session/01010101-0101-0101-0101-010101010101/resume",
            &format!(r#"{{"resume_token":"{credential}","protocol":"orna.present.v1"}}"#),
        ),
        2,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(resumed.status, 200);
    let rotated = token(&resumed);

    let mut stale = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    stale
        .headers
        .push(("authorization".into(), format!("Bearer {credential}")));
    assert_eq!(
        block_on(transport.handle(stale, 3, &mut authority, &mut issuer, &mut deletion,)).status,
        410
    );
    assert_eq!(deletion.calls, 0);

    let mut cookie_only = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    cookie_only
        .headers
        .push(("cookie".into(), format!("orna_session={rotated}")));
    assert_eq!(
        block_on(transport.handle(cookie_only, 3, &mut authority, &mut issuer, &mut deletion,))
            .status,
        401
    );
    assert_eq!(deletion.calls, 0);

    let mut valid = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    valid
        .headers
        .push(("authorization".into(), format!("Bearer {rotated}")));
    assert_eq!(
        block_on(transport.handle(valid, 3, &mut authority, &mut issuer, &mut deletion,)).status,
        204
    );
    assert_eq!(deletion.calls, 1);

    let mut expired_transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut expired_authority = Authority;
    let mut expired_issuer = Issuer(1, None);
    let mut expired_deletion = RecordingDelete::default();
    let expired_created = block_on(expired_transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut expired_authority,
        &mut expired_issuer,
        &mut expired_deletion,
    ));
    let expired_credential = token(&expired_created);
    let mut expired = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    expired.headers.push((
        "authorization".into(),
        format!("Bearer {expired_credential}"),
    ));
    assert_eq!(
        block_on(expired_transport.handle(
            expired,
            100,
            &mut expired_authority,
            &mut expired_issuer,
            &mut expired_deletion,
        ))
        .status,
        410
    );
    assert_eq!(expired_deletion.calls, 0);
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
fn decoded_server_result_is_rejected_before_durable_admission_or_application() {
    let (root, repository) = durable_repository();
    let mut host = durable_host_with_owner(open_durable_state(&repository), [87; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [88; 16],
        now: 1,
    }))
    .unwrap();

    let request = [89; 16];
    let result = unit_result(request, [0; 32])
        .encode(Limits::default().protocol)
        .unwrap();
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame([88; 16], 2, Frame::Binary(result), &mut application,)),
        Err(Error::InvalidMessage)
    );
    assert_eq!(application.calls, 0);
    assert!(
        block_on(
            open_durable_state(&repository).request_status_for_identity(RequestIdentity {
                session_id: [1; 16],
                request_id: request,
            })
        )
        .unwrap()
        .is_none()
    );

    drop(host);
    remove_test_repository(&root);
}

#[test]
fn expired_attachment_rejects_a_valid_binary_request_before_admission() {
    let (root, repository) = durable_repository();
    let mut host = durable_host_with_owner(open_durable_state(&repository), [86; 16]);
    let mut issuer = Issuer(1, None);
    let credential = block_on(host.create(
        CreateRequest {
            id: [1; 16],
            origin: origin(),
            expires_at: 3,
            now: 0,
            subscribe: &subscribe(),
        },
        &mut issuer,
    ))
    .unwrap();
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [85; 16],
        now: 1,
    }))
    .unwrap();

    let request = [84; 16];
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame(
            [85; 16],
            3,
            Frame::Binary(eval([1; 16], request, "1")),
            &mut application,
        )),
        Err(Error::Closed)
    );
    assert_eq!(application.calls, 0);
    assert!(
        block_on(
            open_durable_state(&repository).request_status_for_identity(RequestIdentity {
                session_id: [1; 16],
                request_id: request,
            })
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        block_on(host.dispatch_frame(
            [85; 16],
            4,
            Frame::Binary(eval([1; 16], request, "1")),
            &mut application,
        )),
        Err(Error::Closed)
    );

    drop(host);
    remove_test_repository(&root);
}

#[test]
fn replaced_attachment_cannot_admit_a_frame_or_disconnect_its_replacement() {
    let (root, repository) = durable_repository();
    let mut host = durable_host_with_owner(open_durable_state(&repository), [83; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [82; 16],
        now: 1,
    }))
    .unwrap();
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [81; 16],
        now: 2,
    }))
    .unwrap();

    let stale_request = [80; 16];
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame(
            [82; 16],
            3,
            Frame::Binary(eval([1; 16], stale_request, "1")),
            &mut application,
        )),
        Err(Error::Closed)
    );
    assert_eq!(application.calls, 0);
    assert!(
        block_on(
            open_durable_state(&repository).request_status_for_identity(RequestIdentity {
                session_id: [1; 16],
                request_id: stale_request,
            })
        )
        .unwrap()
        .is_none()
    );

    assert_eq!(
        block_on(host.dispatch_frame(
            [81; 16],
            3,
            Frame::Binary(eval([1; 16], [79; 16], "1")),
            &mut application,
        ))
        .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Accepted)
    );
    assert_eq!(application.calls, 1);

    drop(host);
    remove_test_repository(&root);
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
fn durable_public_request_helpers_reject_without_serving_mutation() {
    let (root, repository) = durable_repository();
    let mut host = durable_host_with_owner(open_durable_state(&repository), [69; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [11; 16],
        now: 1,
    }))
    .unwrap();
    assert_eq!(
        host.reserve_request([1; 16], [68; 16]),
        Err(Error::UnsupportedOperation)
    );
    assert_eq!(
        host.start_request([1; 16], [68; 16]),
        Err(Error::UnsupportedOperation)
    );

    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame(
            [11; 16],
            2,
            Frame::Binary(eval([1; 16], [68; 16], "1")),
            &mut application,
        ))
        .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Accepted)
    );
    assert_eq!(application.calls, 1);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_stale_event_is_rejected_before_request_admission() {
    let (root, repository) = durable_repository();
    let mut host = durable_host_with_owner(open_durable_state(&repository), [70; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [12; 16],
        now: 1,
    }))
    .unwrap();

    let request = [71; 16];
    let frame = event([1; 16], request, [72; 16]);
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame([12; 16], 2, Frame::Binary(frame), &mut application,)),
        Err(Error::Denied)
    );
    assert_eq!(application.calls, 0);
    assert!(
        block_on(
            open_durable_state(&repository).request_status_for_identity(RequestIdentity {
                session_id: [1; 16],
                request_id: request,
            })
        )
        .unwrap()
        .is_none()
    );

    drop(host);
    remove_test_repository(&root);
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
    let first_response_bytes = first
        .response
        .as_ref()
        .unwrap()
        .encode(Limits::default().protocol)
        .unwrap();
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [23; 16],
    };
    let retained_terminal_bytes =
        block_on(open_durable_state(&repository).request_status_for_identity(identity))
            .unwrap()
            .unwrap()
            .terminal_outcome
            .unwrap()
            .as_bytes()
            .to_vec();
    assert_eq!(first_response_bytes, retained_terminal_bytes);
    let observations = block_on(open_durable_state(&repository).run_observations()).unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].invocation_id, [23; 16]);
    assert_eq!(observations[0].status, RunObservationStatus::Completed);
    assert!(!observations[0].live);
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
    let replay = block_on(second_host.dispatch_frame(
        [6; 16],
        4,
        Frame::Binary(request),
        &mut second_application,
    ))
    .unwrap();
    let replay_response_bytes = replay
        .response
        .as_ref()
        .unwrap()
        .encode(Limits::default().protocol)
        .unwrap();
    assert_eq!(replay, first);
    assert_eq!(replay_response_bytes, retained_terminal_bytes);
    assert_eq!(second_application.calls, 0);
    assert_eq!(first_application.calls + second_application.calls, 1);
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

fn assert_durable_application_result_replays_verbatim(
    mut first_application: UnitApplication,
    request_id: [u8; 16],
    expected_status: ResultStatus,
) {
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

    let request = eval([1; 16], request_id, "1");
    let first = block_on(first_host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(request.clone()),
        &mut first_application,
    ))
    .unwrap();
    assert!(matches!(
        first.response.as_ref().unwrap().message,
        Message::Result { status, .. } if status == expected_status
    ));
    assert_eq!(first_application.calls, 1);
    let first_response_bytes = first
        .response
        .as_ref()
        .unwrap()
        .encode(Limits::default().protocol)
        .unwrap();
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id,
    };
    let durable_status =
        block_on(open_durable_state(&repository).request_status_for_identity(identity))
            .unwrap()
            .unwrap();
    assert_eq!(
        durable_status.state,
        orna_runtime_v1::RequestState::Completed
    );
    let retained_terminal_bytes = durable_status.terminal_outcome.unwrap().as_bytes().to_vec();
    assert_eq!(first_response_bytes, retained_terminal_bytes);

    let replay = block_on(first_host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(request.clone()),
        &mut first_application,
    ))
    .unwrap();
    let replay_response_bytes = replay
        .response
        .as_ref()
        .unwrap()
        .encode(Limits::default().protocol)
        .unwrap();
    assert_eq!(replay_response_bytes, retained_terminal_bytes);
    assert_eq!(first_application.calls, 1);
    drop(first_host);

    let mut reconstructed_host = durable_host(open_durable_state(&repository));
    let mut reconstructed_issuer = Issuer(1, None);
    let reconstructed_credential = create(&mut reconstructed_host, &mut reconstructed_issuer);
    block_on(reconstructed_host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &reconstructed_credential,
        attachment: [6; 16],
        now: 3,
    }))
    .unwrap();
    let mut reconstructed_application = UnitApplication::default();
    let reconstructed_replay = block_on(reconstructed_host.dispatch_frame(
        [6; 16],
        4,
        Frame::Binary(request),
        &mut reconstructed_application,
    ))
    .unwrap();
    let reconstructed_replay_bytes = reconstructed_replay
        .response
        .as_ref()
        .unwrap()
        .encode(Limits::default().protocol)
        .unwrap();
    assert_eq!(reconstructed_replay_bytes, retained_terminal_bytes);
    assert_eq!(reconstructed_application.calls, 0);
    assert_eq!(first_application.calls + reconstructed_application.calls, 1);
    let reconstructed_status =
        block_on(open_durable_state(&repository).request_status_for_identity(identity))
            .unwrap()
            .unwrap();
    assert_eq!(
        reconstructed_status.state,
        orna_runtime_v1::RequestState::Completed
    );
    assert_eq!(
        reconstructed_status.terminal_outcome.unwrap().as_bytes(),
        retained_terminal_bytes
    );
    drop(reconstructed_host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_retains_and_replays_a_correlated_semantic_failure_result() {
    assert_durable_application_result_replays_verbatim(
        UnitApplication {
            eval_outcome: UnitEvalOutcome::SemanticFailure,
            ..UnitApplication::default()
        },
        [28; 16],
        ResultStatus::Failure,
    );
}

#[test]
fn durable_runtime_retains_and_replays_a_unit_success_result() {
    assert_durable_application_result_replays_verbatim(
        UnitApplication::default(),
        [29; 16],
        ResultStatus::Success,
    );
}

#[test]
fn durable_live_rejection_marks_its_observed_run_failed() {
    let (root, repository) = durable_repository();
    let mut host = durable_host(open_durable_state(&repository));
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
    let request = eval([1; 16], [24; 16], "1");
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(request), &mut application)),
        Err(Error::ApplicationRejected)
    );
    let observations = block_on(open_durable_state(&repository).run_observations()).unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].invocation_id, [24; 16]);
    assert_eq!(observations[0].status, RunObservationStatus::Failed);
    assert!(observations[0].diagnostic.is_some());
    assert!(!observations[0].live);
    drop(host);
    remove_test_repository(&root);
}

#[test]
#[allow(clippy::too_many_lines)]
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
        let terminal = if target == [35; 16] {
            TerminalOutcome::new(
                Envelope {
                    request: Some(target),
                    watch: None,
                    message: Message::Result {
                        status: ResultStatus::RetainedWithoutValue,
                        value: None,
                        fingerprint,
                        diagnostic: None,
                    },
                    extensions: BTreeMap::new(),
                }
                .encode(Limits::default().protocol)
                .unwrap(),
            )
            .unwrap()
        } else if target == [33; 16] {
            TerminalOutcome::new(
                unit_result(target, fingerprint)
                    .encode(Limits::default().protocol)
                    .unwrap(),
            )
            .unwrap()
        } else if target == [34; 16] {
            TerminalOutcome::new(
                Envelope {
                    request: Some(target),
                    watch: None,
                    message: Message::Result {
                        status: ResultStatus::Cancellation,
                        value: None,
                        fingerprint,
                        diagnostic: None,
                    },
                    extensions: BTreeMap::new(),
                }
                .encode(Limits::default().protocol)
                .unwrap(),
            )
            .unwrap()
        } else {
            TerminalOutcome::new(Vec::new()).unwrap()
        };
        match target[0] {
            33 => {
                block_on(runtime.complete_request(identity, fingerprint, terminal)).unwrap();
            }
            34 => {
                block_on(runtime.cancel_request(identity, fingerprint, terminal)).unwrap();
            }
            35 => {
                let old = block_on(runtime.acquire_lease([91; 16])).unwrap();
                let fence = block_on(runtime.recover_abandoned(old.owner_id, [92; 16])).unwrap();
                block_on(runtime.recover_legacy_running_request(
                    identity,
                    fingerprint,
                    fence,
                    terminal,
                ))
                .unwrap();
            }
            _ => {}
        }
    }
    drop(runtime);

    let mut host = durable_host_after_takeover(
        open_durable_state(&repository),
        [92; 16],
        RequestOwner {
            owner_id: [91; 16],
            epoch: 1,
        },
    );
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
        let expected_state = if state == orna_protocol_v1::RequestState::Running {
            orna_protocol_v1::RequestState::Orphaned
        } else {
            state
        };
        assert!(matches!(
            outcome.response.unwrap().message,
            Message::RequestStatusResult {
                target: returned_target,
                state: returned_state,
                fingerprint: Some(returned_fingerprint),
                result,
            } if returned_target == target
                && returned_state == expected_state
                && returned_fingerprint == fingerprint
                && result.is_some() == (target != [31; 16])
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
    // RequestStatus only reads durable state: neither active rows nor
    // retained terminal rows may invoke the application while reporting it.
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_does_not_replay_a_reserved_request_after_host_reconstruction() {
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
        block_on(
            host.dispatch_frame([7; 16], 2, Frame::Binary(request.clone()), &mut application,)
        )
        .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Accepted)
    );
    assert_eq!(application.calls, 0);

    let duplicate =
        block_on(host.dispatch_frame([7; 16], 2, Frame::Binary(request), &mut application))
            .unwrap();
    assert_eq!(duplicate.outcome, FrameOutcome::Accepted);
    assert!(duplicate.response.is_none());
    assert_eq!(application.calls, 0);

    let status_request = Envelope {
        request: Some([25; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [24; 16],
            fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let status =
        block_on(host.dispatch_frame([7; 16], 3, Frame::Binary(status_request), &mut application))
            .unwrap();
    assert!(matches!(
        status.response.unwrap().message,
        Message::RequestStatusResult {
            target: returned_target,
            state: orna_protocol_v1::RequestState::Reserved,
            fingerprint: Some(returned_fingerprint),
            result: None,
        } if returned_target == [24; 16] && returned_fingerprint == fingerprint
    ));

    let fresh = eval([1; 16], [26; 16], "2");
    assert_eq!(
        block_on(host.dispatch_frame([7; 16], 4, Frame::Binary(fresh), &mut application,))
            .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Accepted)
    );
    assert_eq!(application.calls, 1);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_recovery_does_not_cancel_or_replay_a_running_request() {
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
    let old = block_on(runtime.acquire_lease([71; 16])).unwrap();
    block_on(runtime.recover_abandoned(old.owner_id, [72; 16])).unwrap();
    drop(runtime);

    let mut host = durable_host_after_takeover(
        open_durable_state(&repository),
        [72; 16],
        RequestOwner::from(old),
    );
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
    assert_eq!(application.calls, 0);
    assert!(matches!(
        block_on(host.dispatch_frame([8; 16], 3, Frame::Binary(target), &mut application,))
            .unwrap()
            .outcome,
        FrameOutcome::Accepted
    ));
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_recovers_a_legacy_running_eval_as_uncertain_after_takeover() {
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
    let old = block_on(runtime.acquire_lease([81; 16])).unwrap();
    block_on(runtime.recover_abandoned(old.owner_id, [82; 16])).unwrap();
    drop(runtime);

    let mut host = durable_host_after_takeover(
        open_durable_state(&repository),
        [82; 16],
        RequestOwner::from(old),
    );
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
    assert_eq!(
        Envelope::decode(
            status.terminal_outcome.as_ref().unwrap().as_bytes(),
            Limits::default().protocol,
        )
        .unwrap(),
        first.response.as_ref().unwrap().clone(),
    );

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
    let expected =
        ResultBody::from_result(first.response.as_ref().unwrap(), Limits::default().protocol)
            .unwrap();
    assert!(matches!(
        status_outcome.response.unwrap().message,
        Message::RequestStatusResult {
            target: returned_target,
            state: orna_protocol_v1::RequestState::Orphaned,
            fingerprint: Some(returned_fingerprint),
            result: Some(result),
        } if returned_target == [25; 16]
            && returned_fingerprint == fingerprint
            && result == expected
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
fn durable_runtime_recovers_an_owned_running_request_after_takeover() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [76; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [76; 16],
    };
    let runtime = open_durable_state(&repository);
    let old = block_on(runtime.acquire_lease([73; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        old,
        capability,
    ))
    .unwrap();
    block_on(runtime.recover_abandoned(old.owner_id, [74; 16])).unwrap();
    drop(runtime);

    let mut host = durable_host_after_takeover(
        open_durable_state(&repository),
        [74; 16],
        RequestOwner::from(old),
    );
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
    let fresh = eval([1; 16], [80; 16], "2");
    let fresh_outcome =
        block_on(host.dispatch_frame([9; 16], 2, Frame::Binary(fresh), &mut application)).unwrap();
    assert_eq!(fresh_outcome.outcome, FrameOutcome::Accepted);
    assert_eq!(application.calls, 1);
    let old_status =
        block_on(open_durable_state(&repository).request_status(identity, fingerprint))
            .unwrap()
            .unwrap();
    assert_eq!(old_status.state, RequestState::Orphaned);
    let recovered =
        block_on(host.dispatch_frame([9; 16], 3, Frame::Binary(request.clone()), &mut application))
            .unwrap();
    assert!(matches!(
        recovered.response.as_ref().unwrap().message,
        Message::Result {
            status: ResultStatus::RetainedWithoutValue,
            ..
        }
    ));
    assert_eq!(application.calls, 1);
    assert_eq!(
        block_on(host.dispatch_frame([9; 16], 4, Frame::Binary(request), &mut application)),
        Ok(recovered)
    );
    assert_eq!(application.calls, 1);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_replay_rejects_an_uncertain_payload_for_a_proven_rollback() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [77; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [77; 16],
    };
    let runtime = open_durable_state(&repository);
    let old = block_on(runtime.acquire_lease([73; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        old,
        capability,
    ))
    .unwrap();
    let activation = block_on(runtime.begin_activation()).unwrap();
    let mutation = TableMutation::new([1; 16], "books", vec![1], Some(vec![2])).unwrap();
    assert_eq!(
        block_on(runtime.commit_table_request_activation(
            old,
            identity,
            fingerprint,
            &activation,
            &[mutation],
            [3; 32],
            TerminalOutcome::new(vec![4]).unwrap(),
            &FailAt(FaultPoint::AfterTerminalClaim),
        )),
        Err(RuntimeError::FaultInjected(FaultPoint::AfterTerminalClaim))
    );
    let fence = block_on(runtime.recover_abandoned(old.owner_id, [74; 16])).unwrap();
    let mismatched = TerminalOutcome::new(
        Envelope {
            request: Some([77; 16]),
            watch: None,
            message: Message::Result {
                status: ResultStatus::RetainedWithoutValue,
                value: None,
                fingerprint,
                diagnostic: None,
            },
            extensions: BTreeMap::new(),
        }
        .encode(Limits::default().protocol)
        .unwrap(),
    )
    .unwrap();
    block_on(runtime.recover_running_request_with_outcomes(
        identity,
        fingerprint,
        RequestOwner::from(old),
        fence,
        mismatched.clone(),
        mismatched,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), [74; 16]);
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
        block_on(host.dispatch_frame([9; 16], 2, Frame::Binary(request), &mut application)),
        Err(Error::RuntimeUnavailable)
    );
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_replay_rejects_a_rollback_payload_for_external_uncertainty() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [78; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [78; 16],
    };
    let runtime = open_durable_state(&repository);
    let old = block_on(runtime.acquire_lease([75; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        old,
        capability,
    ))
    .unwrap();
    block_on(runtime.record_external_effect(identity, fingerprint, old)).unwrap();
    let fence = block_on(runtime.recover_abandoned(old.owner_id, [76; 16])).unwrap();
    let mismatched = TerminalOutcome::new(
        Envelope {
            request: Some([78; 16]),
            watch: None,
            message: Message::Result {
                status: ResultStatus::Failure,
                value: None,
                fingerprint,
                diagnostic: None,
            },
            extensions: BTreeMap::new(),
        }
        .encode(Limits::default().protocol)
        .unwrap(),
    )
    .unwrap();
    block_on(runtime.recover_running_request_with_outcomes(
        identity,
        fingerprint,
        RequestOwner::from(old),
        fence,
        mismatched.clone(),
        mismatched,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), [76; 16]);
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
        block_on(host.dispatch_frame([9; 16], 2, Frame::Binary(request), &mut application)),
        Err(Error::RuntimeUnavailable)
    );
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_replays_proven_rollback_as_redacted_orphaned_failure() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [79; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [79; 16],
    };
    let runtime = open_durable_state(&repository);
    let old = block_on(runtime.acquire_lease([77; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        old,
        capability,
    ))
    .unwrap();
    let activation = block_on(runtime.begin_activation()).unwrap();
    let mutation = TableMutation::new([1; 16], "books", vec![1], Some(vec![2])).unwrap();
    assert_eq!(
        block_on(runtime.commit_table_request_activation(
            old,
            identity,
            fingerprint,
            &activation,
            &[mutation],
            [3; 32],
            TerminalOutcome::new(vec![4]).unwrap(),
            &FailAt(FaultPoint::AfterTerminalClaim),
        )),
        Err(RuntimeError::FaultInjected(FaultPoint::AfterTerminalClaim))
    );
    block_on(runtime.recover_abandoned(old.owner_id, [78; 16])).unwrap();
    drop(runtime);

    let mut host = durable_host_after_takeover(
        open_durable_state(&repository),
        [78; 16],
        RequestOwner::from(old),
    );
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [11; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let recovered = block_on(host.dispatch_frame(
        [11; 16],
        2,
        Frame::Binary(request.clone()),
        &mut application,
    ))
    .unwrap();
    assert!(matches!(
        recovered.response.as_ref().unwrap().message,
        Message::Result {
            status: ResultStatus::Failure,
            value: None,
            fingerprint: returned,
            diagnostic: None,
        } if returned == fingerprint
    ));
    assert_eq!(application.calls, 0);

    let durable_status =
        block_on(open_durable_state(&repository).request_status_for_identity(identity))
            .unwrap()
            .unwrap();
    assert_eq!(
        durable_status.state,
        orna_runtime_v1::RequestState::Orphaned
    );
    assert_eq!(
        Envelope::decode(
            durable_status.terminal_outcome.as_ref().unwrap().as_bytes(),
            Limits::default().protocol,
        )
        .unwrap(),
        recovered.response.as_ref().unwrap().clone(),
    );

    let status_request = Envelope {
        request: Some([80; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [79; 16],
            fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let status =
        block_on(host.dispatch_frame([11; 16], 3, Frame::Binary(status_request), &mut application))
            .unwrap();
    let expected = ResultBody::from_result(
        recovered.response.as_ref().unwrap(),
        Limits::default().protocol,
    )
    .unwrap();
    assert!(matches!(
        status.response.unwrap().message,
        Message::RequestStatusResult {
            state: orna_protocol_v1::RequestState::Orphaned,
            fingerprint: Some(returned),
            result: Some(result),
            ..
        } if returned == fingerprint && result == expected
    ));
    assert_eq!(
        block_on(host.dispatch_frame([11; 16], 4, Frame::Binary(request), &mut application)),
        Ok(recovered)
    );
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_reports_a_current_owner_as_active_without_reexecution() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [77; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [77; 16],
    };
    let runtime = open_durable_state(&repository);
    let owner = block_on(runtime.acquire_lease([75; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        owner,
        capability,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), [76; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [10; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let outcome =
        block_on(host.dispatch_frame([10; 16], 2, Frame::Binary(request), &mut application))
            .unwrap();
    assert_eq!(outcome.outcome, FrameOutcome::Accepted);
    assert!(outcome.response.is_none());
    assert_eq!(application.calls, 0);
    let status = block_on(open_durable_state(&repository).request_status(identity, fingerprint))
        .unwrap()
        .unwrap();
    assert_eq!(status.state, orna_runtime_v1::RequestState::Running);
    assert!(status.terminal_outcome.is_none());
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_dispatch_rejects_a_fenced_owner_before_cancellation_callback() {
    let (root, repository) = durable_repository();
    let mut host = durable_host_with_owner(open_durable_state(&repository), [66; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [12; 16],
        now: 1,
    }))
    .unwrap();

    let mut application = UnitApplication::default();
    block_on(host.dispatch_frame(
        [12; 16],
        2,
        Frame::Binary(eval([1; 16], [67; 16], "warm")),
        &mut application,
    ))
    .unwrap();
    assert_eq!(application.calls, 1);

    let target = RequestIdentity {
        session_id: [1; 16],
        request_id: [65; 16],
    };
    let target_fingerprint = request_fingerprint(&eval([1; 16], [65; 16], "target"), [1; 16]);
    let target_runtime = open_durable_state(&repository);
    let old = block_on(target_runtime.acquire_lease([66; 16])).unwrap();
    let (_, capability) =
        block_on(target_runtime.reserve_request_with_admission(target, target_fingerprint))
            .unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(target_runtime.start_request_with_owner_and_admission(
        target,
        target_fingerprint,
        old,
        capability,
    ))
    .unwrap();
    block_on(target_runtime.recover_abandoned(old.owner_id, [68; 16])).unwrap();
    drop(target_runtime);

    let cancellation = cancel_request([64; 16], [65; 16]);
    assert_eq!(
        block_on(host.dispatch_frame([12; 16], 3, Frame::Binary(cancellation), &mut application,)),
        Err(Error::RuntimeUnavailable)
    );
    assert_eq!(application.calls, 1);

    let status =
        block_on(open_durable_state(&repository).request_status(target, target_fingerprint))
            .unwrap()
            .unwrap();
    assert_eq!(status.state, orna_runtime_v1::RequestState::Running);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_rejects_a_stale_terminal_owner_transition() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [78; 16],
    };
    let fingerprint = [78; 32];
    let old = block_on(runtime.acquire_lease([77; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        old,
        capability,
    ))
    .unwrap();
    block_on(runtime.recover_abandoned(old.owner_id, [78; 16])).unwrap();
    assert_eq!(
        block_on(runtime.complete_request_with_owner(
            identity,
            fingerprint,
            old,
            TerminalOutcome::new(Vec::new()).unwrap(),
        )),
        Err(RuntimeError::OwnerLost)
    );
    assert!(matches!(
        block_on(runtime.request_status(identity, fingerprint)).unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Running
    ));
    drop(runtime);
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
fn completion_race_rejects_a_terminal_winner_with_mismatched_request_bytes() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let owner = [91; 16];
    let mut host = durable_host_with_owner(open_durable_state(&repository), owner);
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

    let request = eval([1; 16], [92; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [92; 16],
    };
    let mut application = CompetingTerminalApplication {
        repository: repository.clone(),
        owner,
        calls: 0,
    };
    assert_eq!(
        block_on(host.dispatch_frame([9; 16], 2, Frame::Binary(request.clone()), &mut application)),
        Err(Error::RuntimeUnavailable)
    );
    assert_eq!(application.calls, 1);

    let status = block_on(runtime.request_status(identity, fingerprint))
        .unwrap()
        .unwrap();
    assert_eq!(status.state, orna_runtime_v1::RequestState::Completed);
    assert_eq!(
        Envelope::decode(
            status.terminal_outcome.unwrap().as_bytes(),
            Limits::default().protocol,
        )
        .unwrap()
        .request,
        Some([99; 16])
    );
    assert_eq!(
        block_on(host.dispatch_frame([9; 16], 3, Frame::Binary(request), &mut application)),
        Err(Error::RuntimeUnavailable)
    );
    assert_eq!(application.calls, 1);
    drop(host);
    drop(runtime);
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
        block_on(host.delete(
            DeleteRequest {
                id: [1; 16],
                origin: &origin(),
                credential: &credential,
                now: 1,
            },
            &mut Delete(false),
        )),
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

#[test]
fn delete_cancels_durable_session_work_before_returning_success() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [91; 16],
    };
    let fingerprint = [92; 32];
    let owner = [93; 16];
    let lease = block_on(runtime.acquire_lease(owner)).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        lease,
        capability,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), owner);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    let origin = origin();
    let mut deletion = RecordingDelete::default();
    let mut children = RecordingChildren::default();
    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(deletion.calls, 1);
    assert_eq!(children.calls, 1);
    assert_eq!(children.requests, vec![identity]);
    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(deletion.calls, 1);
    assert_eq!(children.calls, 1);
    assert!(matches!(
        block_on(open_durable_state(&repository).request_status(identity, fingerprint)).unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Cancelled
    ));
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin,
            credential: &credential,
            attachment: [94; 16],
            now: 1,
        })),
        Err(Error::Closed)
    );
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn delete_enumerates_reserved_durable_work_before_joining_children() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [94; 16],
    };
    let fingerprint = [95; 32];
    block_on(runtime.reserve_request(identity, fingerprint)).unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), [96; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    let origin = origin();
    let mut deletion = RecordingDelete::default();
    let mut children = RecordingChildren::default();
    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(children.requests, vec![identity]);
    assert!(matches!(
        block_on(open_durable_state(&repository).request_status(identity, fingerprint)).unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Cancelled
    ));
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn failed_child_join_after_durable_cancellation_never_reports_delete_success() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let reserved = RequestIdentity {
        session_id: [1; 16],
        request_id: [96; 16],
    };
    let running = RequestIdentity {
        session_id: [1; 16],
        request_id: [97; 16],
    };
    let reserved_fingerprint = [98; 32];
    let running_fingerprint = [99; 32];
    let owner = [100; 16];
    let lease = block_on(runtime.acquire_lease(owner)).unwrap();
    block_on(runtime.reserve_request(reserved, reserved_fingerprint)).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(running, running_fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        running,
        running_fingerprint,
        lease,
        capability,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), owner);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    let origin = origin();
    let mut deletion = RecordingDelete::default();
    let mut children = RecordingChildren {
        fail: true,
        ..RecordingChildren::default()
    };
    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        400
    );
    assert_eq!(deletion.calls, 0);
    assert_eq!(children.calls, 1);
    assert_eq!(children.requests, vec![reserved, running]);
    // A failed join leaves the session fenced. Retrying the same authenticated
    // DELETE must never turn the prior failed cleanup into an idempotent 204.
    assert_ne!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(deletion.calls, 0);
    assert_eq!(children.calls, 1);
    for (identity, fingerprint) in [
        (reserved, reserved_fingerprint),
        (running, running_fingerprint),
    ] {
        assert!(matches!(
            block_on(open_durable_state(&repository).request_status(identity, fingerprint))
                .unwrap(),
            Some(status) if status.state == orna_runtime_v1::RequestState::Cancelled
        ));
    }
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin,
            credential: &credential,
            attachment: [101; 16],
            now: 1,
        })),
        Err(Error::Closed)
    );
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_admission_during_child_drain_is_rejected_by_the_close_fence() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let initial = RequestIdentity {
        session_id: [1; 16],
        request_id: [102; 16],
    };
    let late = RequestIdentity {
        session_id: [1; 16],
        request_id: [103; 16],
    };
    let initial_fingerprint = [104; 32];
    let late_fingerprint = [105; 32];
    block_on(runtime.reserve_request(initial, initial_fingerprint)).unwrap();

    let mut host = durable_host_with_owner(open_durable_state(&repository), [106; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    let origin = origin();
    let mut deletion = RecordingDelete::default();
    let mut children = LateAdmissionChildren {
        runtime: &runtime,
        identity: late,
        fingerprint: late_fingerprint,
        calls: 0,
    };
    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(children.calls, 1);
    assert_eq!(deletion.calls, 1);
    assert!(matches!(
        block_on(runtime.request_status(initial, initial_fingerprint)).unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Cancelled
    ));
    assert_eq!(
        block_on(runtime.request_status(late, late_fingerprint)).unwrap(),
        None
    );
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin,
            credential: &credential,
            attachment: [107; 16],
            now: 1,
        })),
        Err(Error::Closed)
    );
    drop(host);
    drop(runtime);
    remove_test_repository(&root);
}

#[test]
fn application_admission_requires_an_explicit_delete_join_boundary() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    let origin = origin();
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin,
        credential: &credential,
        attachment: [97; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    block_on(host.dispatch_frame(
        [97; 16],
        2,
        Frame::Binary(eval([1; 16], [98; 16], "1")),
        &mut application,
    ))
    .unwrap();

    let mut deletion = RecordingDelete::default();
    assert_eq!(
        block_on(host.http_delete(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 2,
            },
            &mut deletion,
        ))
        .status,
        400
    );
    assert_eq!(deletion.calls, 0);

    let mut children = RecordingChildren::default();
    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 2,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(children.calls, 1);
    assert!(children.requests.is_empty());
}

#[test]
fn failed_durable_drain_never_reports_delete_success() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [95; 16],
    };
    let fingerprint = [96; 32];
    let lease = block_on(runtime.acquire_lease([97; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        lease,
        capability,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), [98; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    let origin = origin();
    let mut deletion = RecordingDelete::default();
    let mut children = RecordingChildren::default();
    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        503
    );
    assert_eq!(deletion.calls, 0);
    assert_eq!(children.calls, 0);
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin,
            credential: &credential,
            attachment: [99; 16],
            now: 1,
        })),
        Err(Error::Closed)
    );
    assert!(matches!(
        block_on(open_durable_state(&repository).request_status(identity, fingerprint)).unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Running
    ));
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn expired_delete_cannot_cancel_durable_session_work() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [100; 16],
    };
    let fingerprint = [101; 32];
    let owner = [102; 16];
    let lease = block_on(runtime.acquire_lease(owner)).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        lease,
        capability,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), owner);
    let mut issuer = Issuer(1, None);
    let credential = block_on(host.create(
        CreateRequest {
            id: [1; 16],
            origin: origin(),
            expires_at: 1,
            now: 0,
            subscribe: &subscribe(),
        },
        &mut issuer,
    ))
    .unwrap();
    let origin = origin();
    let mut deletion = RecordingDelete::default();
    assert_eq!(
        block_on(host.http_delete(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
        ))
        .status,
        410
    );
    assert_eq!(deletion.calls, 0);
    assert!(matches!(
        block_on(open_durable_state(&repository).request_status(identity, fingerprint)).unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Running
    ));
    drop(host);
    remove_test_repository(&root);
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
fn websocket_upgrade(session: u8, credential: &str) -> WireRequest {
    let mut request = wire("GET", &format!("/orna/live/{}", uuid(session)), "");
    request.headers.extend([
        ("connection".into(), "Upgrade".into()),
        ("upgrade".into(), "websocket".into()),
        ("sec-websocket-version".into(), "13".into()),
        (
            "sec-websocket-key".into(),
            "dGhlIHNhbXBsZSBub25jZQ==".into(),
        ),
        ("sec-websocket-protocol".into(), SUBPROTOCOL.into()),
        ("cookie".into(), format!("orna_session={credential}")),
    ]);
    request
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

fn unmasked(fin: bool, opcode: u8, body: &[u8]) -> Vec<u8> {
    assert!(body.len() < 126);
    let length = u8::try_from(body.len()).expect("test frame is short");
    let mut frame = vec![(if fin { 128 } else { 0 }) | opcode, length];
    frame.extend_from_slice(body);
    frame
}

fn masked_with_length_code(
    fin: bool,
    opcode: u8,
    body: &[u8],
    length_code: u8,
    encoded_length: u64,
) -> Vec<u8> {
    let key = [1, 2, 3, 4];
    let mut frame = vec![(if fin { 128 } else { 0 }) | opcode, 128 | length_code];
    match length_code {
        126 => frame.extend_from_slice(&(encoded_length as u16).to_be_bytes()),
        127 => frame.extend_from_slice(&encoded_length.to_be_bytes()),
        _ => unreachable!("test helper only encodes extended lengths"),
    }
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
    let mut children = RecordingChildren::default();
    assert_eq!(
        block_on(transport.handle_with_children(
            deleted,
            2,
            &mut authority,
            &mut issuer,
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
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
            WebSocketOutput::Close { code: None }
        ]
    );
    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &masked(true, 2, &message))),
        Err(Error::Closed)
    );
    assert_eq!(
        block_on(transport.receive(
            &mut WebSocketState::new([5; 16]),
            2,
            &masked(true, 1, b"text"),
        )),
        Ok(vec![WebSocketOutput::Close { code: Some(1003) }])
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
        encode_websocket_output(
            &WebSocketOutput::Close { code: None },
            TransportLimits::default()
        )
        .unwrap(),
        Some(vec![0x88, 0])
    );
    assert_eq!(
        encode_websocket_output(
            &WebSocketOutput::Close { code: Some(1002) },
            TransportLimits::default()
        )
        .unwrap(),
        Some(vec![0x88, 2, 0x03, 0xea])
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
            Ok(vec![WebSocketOutput::Close { code: Some(1002) }])
        );
    }
}

#[test]
fn websocket_input_rejects_noncanonical_extended_lengths() {
    let cases = [
        (126_u8, 125_u64, vec![7; 125]),
        (127_u8, 126_u64, vec![7; 126]),
        (127_u8, u64::from(u16::MAX), vec![7; u16::MAX as usize]),
    ];
    for (length_code, encoded_length, body) in cases {
        let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
        let mut socket = WebSocketState::new([5; 16]);
        assert_eq!(
            block_on(transport.receive(
                &mut socket,
                2,
                &masked_with_length_code(true, 2, &body, length_code, encoded_length),
            )),
            Ok(vec![WebSocketOutput::Close { code: Some(1002) }])
        );
    }

    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    assert_eq!(
        block_on(transport.receive(
            &mut socket,
            2,
            &masked_with_length_code(true, 2, &[], 127, 1_u64 << 63),
        )),
        Ok(vec![WebSocketOutput::Close { code: Some(1002) }])
    );
}

#[test]
fn websocket_input_processes_coalesced_frames_with_per_frame_limits() {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    let mut host_limits = Limits::default();
    host_limits.protocol.max_message_bytes = 2;
    let mut transport = LiveTransport::new(
        LiveHost::new(
            host_limits,
            boundary,
            Serving::new(ServingLimits::default()).unwrap(),
        )
        .unwrap(),
        TransportLimits {
            max_frame_bytes: 2,
            ..TransportLimits::default()
        },
    )
    .unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    let mut bytes = masked(true, 9, &[1]);
    bytes.extend(masked(true, 9, &[2]));
    bytes.extend(masked(true, 9, &[3]));

    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &bytes)),
        Ok(vec![
            WebSocketOutput::Pong(vec![1]),
            WebSocketOutput::Pong(vec![2]),
            WebSocketOutput::Pong(vec![3]),
        ])
    );
}

#[test]
fn websocket_input_rejects_oversized_control_frames_as_invalid() {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    let mut host_limits = Limits::default();
    host_limits.protocol.max_message_bytes = 2;
    let mut transport = LiveTransport::new(
        LiveHost::new(
            host_limits,
            boundary,
            Serving::new(ServingLimits::default()).unwrap(),
        )
        .unwrap(),
        TransportLimits {
            max_frame_bytes: 2,
            ..TransportLimits::default()
        },
    )
    .unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    let frame = masked_with_length_code(true, 9, &[7; 126], 126, 126);

    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &frame)),
        Ok(vec![WebSocketOutput::Close { code: Some(1002) }])
    );
}

#[test]
fn websocket_input_malformed_frame_emits_protocol_close_once() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    let malformed = masked(true, 0, b"");

    let output = block_on(transport.receive(&mut socket, 2, &malformed)).unwrap();
    assert_eq!(output, vec![WebSocketOutput::Close { code: Some(1002) }]);
    assert_eq!(
        encode_websocket_output(&output[0], TransportLimits::default()).unwrap(),
        Some(vec![0x88, 2, 0x03, 0xea])
    );
    assert_eq!(transport.take_retired_attachments(), Vec::<[u8; 16]>::new());
    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &masked(true, 9, b"p"))),
        Err(Error::Closed)
    );
}

#[test]
fn websocket_input_malformed_application_message_closes_and_retires_attachment() {
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
    let credential = token(&created);
    assert_eq!(
        block_on(transport.upgrade(websocket_upgrade(1, &credential), [5; 16], 1)).status,
        101
    );

    let mut socket = WebSocketState::new([5; 16]);
    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &masked(true, 2, &[0xff]))),
        Ok(vec![WebSocketOutput::Close { code: Some(1002) }])
    );
    assert_eq!(
        block_on(transport.close_attachment([5; 16], 2)),
        Err(Error::Closed)
    );
}

#[test]
fn websocket_input_coalesced_malformed_frame_closes_after_prior_output() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    let mut bytes = masked(true, 9, b"p");
    bytes.extend(masked(true, 0, b""));

    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &bytes)),
        Ok(vec![
            WebSocketOutput::Pong(vec![b'p']),
            WebSocketOutput::Close { code: Some(1002) },
        ])
    );
}

#[test]
fn websocket_input_limit_emits_message_too_big_close() {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    let mut host_limits = Limits::default();
    host_limits.protocol.max_message_bytes = 2;
    let host = LiveHost::new(
        host_limits,
        boundary,
        Serving::new(ServingLimits::default()).unwrap(),
    )
    .unwrap();
    let limits = TransportLimits {
        max_frame_bytes: 2,
        max_outgoing_bytes: 256,
        ..TransportLimits::default()
    };
    let mut transport = LiveTransport::new(host, limits).unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    let output = block_on(transport.receive(&mut socket, 2, &masked(true, 2, &[7; 3]))).unwrap();
    assert_eq!(output, vec![WebSocketOutput::Close { code: Some(1009) }]);
    assert_eq!(
        encode_websocket_output(&output[0], limits).unwrap(),
        Some(vec![0x88, 2, 0x03, 0xf1])
    );
    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &masked(true, 9, b"p"))),
        Err(Error::Closed)
    );
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
