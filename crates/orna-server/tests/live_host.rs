use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::{FutureExt, executor::block_on};
use orna_protocol_v1::{
    DatabaseContext, Envelope, Limits as ProtocolLimits, Message, PresentationContext,
    ResultStatus, canonical_request_fingerprint,
};
use orna_repository_v1::initialize_repository;
use orna_runtime_v1::{RequestIdentity, RequestState, RuntimeIdentity, RuntimeState};
use orna_server::{LiveHostError, LiveOnceHost};

struct TemporaryRepository {
    path: PathBuf,
}

impl TemporaryRepository {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "orna-live-host-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryRepository {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).unwrap();
    }
}

fn request(address: std::net::SocketAddr, database: &str) -> String {
    let body = format!(r#"{{"database":"{database}","protocol":"orna.present.v1"}}"#);
    format!(
        "POST /orna/session HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        address.port(),
        body.len()
    )
}

fn delete_request(address: std::net::SocketAddr, session: &str, token: &str) -> String {
    format!(
        "DELETE /orna/session/{session} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nAuthorization: Bearer {token}\r\nContent-Length: 0\r\n\r\n",
        address.port()
    )
}

fn read_response(stream: &mut TcpStream) -> String {
    let mut response = Vec::new();
    let mut byte = [0; 1];
    while !response.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        response.extend_from_slice(&byte);
    }
    let header = String::from_utf8(response).unwrap();
    let length = header
        .lines()
        .find_map(|line| line.strip_prefix("Content-Length: "))
        .unwrap_or("0")
        .parse::<usize>()
        .unwrap();
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    header + &String::from_utf8(body).unwrap()
}

fn json_field(response: &str, field: &str) -> String {
    let prefix = format!(r#""{field}":""#);
    response
        .split_once(&prefix)
        .and_then(|(_, rest)| rest.split_once('"').map(|(value, _)| value.to_owned()))
        .unwrap()
}

fn masked(fin: bool, opcode: u8, body: &[u8]) -> Vec<u8> {
    assert!(body.len() <= u16::MAX as usize);
    let key = [1, 2, 3, 4];
    let mut frame = vec![(if fin { 0x80 } else { 0 }) | opcode];
    if body.len() < 126 {
        frame.push(0x80 | u8::try_from(body.len()).unwrap());
    } else {
        frame.push(0x80 | 126);
        frame.extend(u16::try_from(body.len()).unwrap().to_be_bytes());
    }
    frame.extend(key);
    frame.extend(
        body.iter()
            .enumerate()
            .map(|(index, byte)| byte ^ key[index % key.len()]),
    );
    frame
}

fn uuid_bytes(value: &str) -> [u8; 16] {
    let hex = value.replace('-', "");
    assert_eq!(hex.len(), 32);
    let mut bytes = [0; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).unwrap();
    }
    bytes
}

fn stored_runtime_identity(database_id: [u8; 16]) -> (RuntimeIdentity, [u8; 32]) {
    let mut repository_id = database_id;
    for (index, byte) in repository_id.iter_mut().enumerate() {
        let rotation = u32::try_from(index % 7 + 1).unwrap();
        let salt = u8::try_from(index).unwrap();
        *byte = byte.rotate_left(rotation) ^ (0x5a_u8.wrapping_add(salt));
    }
    if repository_id == [0; 16] {
        repository_id[0] = 1;
    }
    let mut digest = [0; 32];
    digest[..16].copy_from_slice(&database_id);
    digest[16..].copy_from_slice(&repository_id);
    (
        RuntimeIdentity {
            database_id,
            repository_id,
        },
        digest,
    )
}

fn eval_payload(session: [u8; 16], request: [u8; 16], database: [u8; 16], source: &str) -> Vec<u8> {
    eval_payload_at(session, request, database, None, source)
}

fn eval_payload_at(
    session: [u8; 16],
    request: [u8; 16],
    database: [u8; 16],
    snapshot: Option<orna_foundation_v1::CanonicalSnapshot>,
    source: &str,
) -> Vec<u8> {
    let mut envelope = Envelope {
        request: Some(request),
        watch: None,
        message: Message::Eval {
            source: source.into(),
            database: DatabaseContext { database, snapshot },
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "terminal/dark".into(),
                supported_kinds: vec![],
            },
            fingerprint: [0; 32],
        },
        extensions: std::collections::BTreeMap::new(),
    };
    let fingerprint =
        canonical_request_fingerprint(session, &envelope, ProtocolLimits::default()).unwrap();
    if let Message::Eval {
        fingerprint: sent, ..
    } = &mut envelope.message
    {
        *sent = fingerprint;
    }
    envelope.encode(ProtocolLimits::default()).unwrap()
}

fn watch_payload(request: [u8; 16], database: [u8; 16], source: &str) -> Vec<u8> {
    Envelope {
        request: Some(request),
        watch: None,
        message: Message::Watch {
            source: source.into(),
            database: DatabaseContext {
                database,
                snapshot: None,
            },
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "terminal/dark".into(),
                supported_kinds: vec![],
            },
            refresh_floor: None,
        },
        extensions: std::collections::BTreeMap::new(),
    }
    .encode(ProtocolLimits::default())
    .unwrap()
}

fn websocket_result(response: &[u8]) -> Envelope {
    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    assert!(
        response.len() > header_end,
        "WebSocket closed before a binary response: {}",
        String::from_utf8_lossy(response)
    );
    assert_eq!(response[header_end], 0x82);
    let (length, offset) = match response[header_end + 1] {
        length @ 0..=125 => (usize::from(length), header_end + 2),
        126 => (
            usize::from(u16::from_be_bytes([
                response[header_end + 2],
                response[header_end + 3],
            ])),
            header_end + 4,
        ),
        _ => panic!("unexpected WebSocket result length"),
    };
    Envelope::decode(
        &response[offset..offset + length],
        ProtocolLimits::default(),
    )
    .unwrap()
}

fn websocket_frame(stream: &mut TcpStream) -> Envelope {
    let mut header = [0; 2];
    stream.read_exact(&mut header).unwrap();
    assert_eq!(header[0], 0x82);
    assert_eq!(header[1] & 0x80, 0);
    let length = match header[1] {
        length @ 0..=125 => usize::from(length),
        126 => {
            let mut bytes = [0; 2];
            stream.read_exact(&mut bytes).unwrap();
            usize::from(u16::from_be_bytes(bytes))
        }
        127 => {
            let mut bytes = [0; 8];
            stream.read_exact(&mut bytes).unwrap();
            usize::try_from(u64::from_be_bytes(bytes)).unwrap()
        }
        _ => unreachable!(),
    };
    let mut body = vec![0; length];
    stream.read_exact(&mut body).unwrap();
    Envelope::decode(&body, ProtocolLimits::default()).unwrap()
}

fn websocket_upgrade(address: std::net::SocketAddr, session: &str, token: &str) -> TcpStream {
    let mut websocket = TcpStream::connect(address).unwrap();
    let handshake = format!(
        "GET /orna/live/{session} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: orna.present.v1\r\nCookie: orna_session={token}\r\n\r\n",
        address.port()
    );
    websocket.write_all(handshake.as_bytes()).unwrap();
    let mut response = Vec::new();
    let mut byte = [0; 1];
    while !response.ends_with(b"\r\n\r\n") {
        websocket.read_exact(&mut byte).unwrap();
        response.extend_from_slice(&byte);
    }
    assert!(response.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
    websocket
}

fn websocket_eval(
    address: std::net::SocketAddr,
    session: &str,
    token: &str,
    database: &str,
    request: [u8; 16],
    source: &str,
) -> Envelope {
    websocket_eval_at(address, session, token, database, request, None, source)
}

fn websocket_eval_at(
    address: std::net::SocketAddr,
    session: &str,
    token: &str,
    database: &str,
    request: [u8; 16],
    snapshot: Option<orna_foundation_v1::CanonicalSnapshot>,
    source: &str,
) -> Envelope {
    let mut websocket = TcpStream::connect(address).unwrap();
    let handshake = format!(
        "GET /orna/live/{session} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: orna.present.v1\r\nCookie: orna_session={token}\r\n\r\n",
        address.port()
    );
    let mut input = handshake.into_bytes();
    input.extend(masked(
        true,
        2,
        &eval_payload_at(
            uuid_bytes(session),
            request,
            uuid_bytes(database),
            snapshot,
            source,
        ),
    ));
    input.extend(masked(true, 8, b""));
    websocket.write_all(&input).unwrap();
    websocket.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    websocket.read_to_end(&mut response).unwrap();
    assert!(response.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
    websocket_result(&response)
}

fn websocket_watch(
    address: std::net::SocketAddr,
    session: &str,
    token: &str,
    database: &str,
    request: [u8; 16],
    source: &str,
) -> Envelope {
    let mut websocket = TcpStream::connect(address).unwrap();
    let handshake = format!(
        "GET /orna/live/{session} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: orna.present.v1\r\nCookie: orna_session={token}\r\n\r\n",
        address.port()
    );
    let mut input = handshake.into_bytes();
    input.extend(masked(
        true,
        2,
        &watch_payload(request, uuid_bytes(database), source),
    ));
    input.extend(masked(true, 8, b""));
    websocket.write_all(&input).unwrap();
    websocket.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    websocket.read_to_end(&mut response).unwrap();
    assert!(response.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
    websocket_result(&response)
}

#[test]
fn loopback_host_creates_a_session_from_a_real_repository() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    let database = initialized.metadata().database_id().to_string();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();
    assert!(address.ip().is_loopback());

    let request_database = database.clone();
    let client = std::thread::spawn(move || {
        let mut client = TcpStream::connect(address).unwrap();
        client
            .write_all(request(address, &request_database).as_bytes())
            .unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        response
    });
    let result = host.serve();
    let response = client.join().unwrap();
    assert!(result.is_ok(), "{result:?}: {response}");

    assert!(response.starts_with("HTTP/1.1 201 Created\r\n"));
    assert!(response.contains(&format!(r#""database":"{database}""#)));
    assert!(response.contains(r#""runtime":""#));
    assert!(response.contains("set-cookie: orna_session="));
    assert!(response.contains("resume_token"));
}

#[test]
fn loopback_host_rejects_a_session_for_another_database() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();
    let client = std::thread::spawn(move || {
        let mut client = TcpStream::connect(address).unwrap();
        client
            .write_all(request(address, "00000000-0000-0000-0000-000000000001").as_bytes())
            .unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        response
    });
    let result = host.serve();
    let response = client.join().unwrap();
    assert!(result.is_ok(), "{result:?}: {response}");

    assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
    assert!(response.contains("live.database_unavailable"));
}

#[test]
fn loopback_host_cancellation_releases_a_listener_before_accept() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();

    assert_eq!(
        host.serve_with_cancellation(futures::future::ready(())),
        Err(LiveHostError::Cancelled)
    );
    let _released = TcpListener::bind(address).unwrap();
}

#[test]
fn loopback_host_cancellation_closes_a_stalled_connection() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();
    let request_id = [0x51; 16];
    let (sender, receiver) = futures::channel::oneshot::channel();
    let client = std::thread::spawn(move || {
        let _client = TcpStream::connect(address).unwrap();
        sender.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    });

    assert_eq!(
        host.serve_with_cancellation(receiver.map(|_| ())),
        Err(LiveHostError::Cancelled)
    );
    client.join().unwrap();
    let _released = TcpListener::bind(address).unwrap();
}

#[test]
fn loopback_host_runs_create_resume_and_delete_on_one_connection() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    let database = initialized.metadata().database_id().to_string();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();
    let request_database = database.clone();
    let client = std::thread::spawn(move || {
        let mut client = TcpStream::connect(address).unwrap();
        client
            .write_all(request(address, &request_database).as_bytes())
            .unwrap();
        let created = read_response(&mut client);
        assert!(created.starts_with("HTTP/1.1 201 Created\r\n"));
        let session = json_field(&created, "session");
        let first_token = json_field(&created, "resume_token");

        let body = format!(r#"{{"resume_token":"{first_token}","protocol":"orna.present.v1"}}"#);
        let resume = format!(
            "POST /orna/session/{session}/resume HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            address.port(),
            body.len()
        );
        client.write_all(resume.as_bytes()).unwrap();
        let resumed = read_response(&mut client);
        assert!(resumed.starts_with("HTTP/1.1 200 OK\r\n"));
        assert_eq!(json_field(&resumed, "session"), session);
        let second_token = json_field(&resumed, "resume_token");
        assert_ne!(second_token, first_token);

        let delete = format!(
            "DELETE /orna/session/{session} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nAuthorization: Bearer {second_token}\r\nContent-Length: 0\r\n\r\n",
            address.port()
        );
        client.write_all(delete.as_bytes()).unwrap();
        let deleted = read_response(&mut client);
        assert!(deleted.starts_with("HTTP/1.1 204 No Content\r\n"));
        client.shutdown(Shutdown::Write).unwrap();
    });
    assert!(host.serve().is_ok());
    client.join().unwrap();
}

#[test]
fn loopback_host_reuses_session_state_across_connections_until_cancelled() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    let database = initialized.metadata().database_id().to_string();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();
    let request_database = database.clone();
    let (sender, receiver) = futures::channel::oneshot::channel();
    let client = std::thread::spawn(move || {
        let mut first = TcpStream::connect(address).unwrap();
        first
            .write_all(request(address, &request_database).as_bytes())
            .unwrap();
        first.shutdown(Shutdown::Write).unwrap();
        let mut first_response = Vec::new();
        first.read_to_end(&mut first_response).unwrap();
        assert!(first_response.starts_with(b"HTTP/1.1 201 Created\r\n"));

        let mut second = TcpStream::connect(address).unwrap();
        second
            .write_all(request(address, "00000000-0000-0000-0000-000000000001").as_bytes())
            .unwrap();
        second.shutdown(Shutdown::Write).unwrap();
        let mut second_response = Vec::new();
        second.read_to_end(&mut second_response).unwrap();
        assert!(second_response.starts_with(b"HTTP/1.1 404 Not Found\r\n"));
        sender.send(()).unwrap();
    });

    assert_eq!(
        host.serve_until_cancellation(receiver.map(|_| ())),
        Err(LiveHostError::Cancelled)
    );
    client.join().unwrap();
    let _released = TcpListener::bind(address).unwrap();
}

#[test]
fn loopback_host_serves_websocket_and_resumes_the_session_after_close() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    let database = initialized.metadata().database_id().to_string();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();
    let (sender, receiver) = futures::channel::oneshot::channel();
    let client = std::thread::spawn(move || {
        let mut create = TcpStream::connect(address).unwrap();
        create
            .write_all(request(address, &database).as_bytes())
            .unwrap();
        let created = read_response(&mut create);
        assert!(created.starts_with("HTTP/1.1 201 Created\r\n"));
        let session = json_field(&created, "session");
        let token = json_field(&created, "resume_token");
        create.shutdown(Shutdown::Write).unwrap();
        let mut ignored = Vec::new();
        create.read_to_end(&mut ignored).unwrap();

        let mut websocket = TcpStream::connect(address).unwrap();
        let handshake = format!(
            "GET /orna/live/{session} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: orna.present.v1\r\nCookie: orna_session={token}\r\n\r\n",
            address.port()
        );
        let mut upgrade = handshake.into_bytes();
        upgrade.extend(masked(true, 9, b"hi"));
        upgrade.extend(masked(true, 8, b""));
        websocket.write_all(&upgrade).unwrap();
        websocket.shutdown(Shutdown::Write).unwrap();
        let mut websocket_response = Vec::new();
        websocket.read_to_end(&mut websocket_response).unwrap();
        let header_end = websocket_response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap();
        assert!(websocket_response.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
        assert_eq!(&websocket_response[header_end + 4..], b"\x8a\x02hi\x88\x00");

        let body = format!(r#"{{"resume_token":"{token}","protocol":"orna.present.v1"}}"#);
        let mut resume = TcpStream::connect(address).unwrap();
        let resume_request = format!(
            "POST /orna/session/{session}/resume HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            address.port(),
            body.len()
        );
        resume.write_all(resume_request.as_bytes()).unwrap();
        let resumed = read_response(&mut resume);
        assert!(resumed.starts_with("HTTP/1.1 200 OK\r\n"));
        assert_eq!(json_field(&resumed, "session"), session);
        resume.shutdown(Shutdown::Write).unwrap();
        let mut ignored = Vec::new();
        resume.read_to_end(&mut ignored).unwrap();
        sender.send(()).unwrap();
    });

    assert_eq!(
        host.serve_until_cancellation(receiver.map(|_| ())),
        Err(LiveHostError::Cancelled)
    );
    client.join().unwrap();
    let _released = TcpListener::bind(address).unwrap();
}

#[test]
fn loopback_host_closes_an_invalid_client_direction_envelope_with_1002() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    let database = initialized.metadata().database_id().to_string();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();
    let (sender, receiver) = futures::channel::oneshot::channel();
    let client = std::thread::spawn(move || {
        let mut create = TcpStream::connect(address).unwrap();
        create
            .write_all(request(address, &database).as_bytes())
            .unwrap();
        let created = read_response(&mut create);
        let session = json_field(&created, "session");
        let token = json_field(&created, "resume_token");
        create.shutdown(Shutdown::Write).unwrap();
        let mut ignored = Vec::new();
        create.read_to_end(&mut ignored).unwrap();

        let invalid = Envelope {
            request: Some(request_id),
            watch: None,
            message: Message::Result {
                status: ResultStatus::Success,
                value: Some(orna_foundation_v1::CanonicalValue::unit()),
                fingerprint: [0; 32],
                diagnostic: None,
            },
            extensions: std::collections::BTreeMap::new(),
        }
        .encode(ProtocolLimits::default())
        .unwrap();
        let mut websocket = websocket_upgrade(address, &session, &token);
        websocket.write_all(&masked(true, 2, &invalid)).unwrap();
        websocket.shutdown(Shutdown::Write).unwrap();
        let mut close = Vec::new();
        websocket.read_to_end(&mut close).unwrap();
        assert_eq!(close, b"\x88\x02\x03\xea");
        sender.send(()).unwrap();
        session
    });

    assert_eq!(
        host.serve_until_cancellation(receiver.map(|_| ())),
        Err(LiveHostError::Cancelled)
    );
    let session = client.join().unwrap();
    let (identity, digest) = stored_runtime_identity(uuid_bytes(&database));
    let state = block_on(RuntimeState::open(
        initialized.repository(),
        identity,
        digest,
    ))
    .unwrap();
    assert!(
        block_on(state.request_status_for_identity(RequestIdentity {
            session_id: uuid_bytes(&session),
            request_id,
        }))
        .unwrap()
        .is_none()
    );
    let _released = TcpListener::bind(address).unwrap();
}

#[test]
fn loopback_host_retires_an_open_websocket_before_resume_completes() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    let database = initialized.metadata().database_id().to_string();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();
    let (sender, receiver) = futures::channel::oneshot::channel();
    let client = std::thread::spawn(move || {
        let mut create = TcpStream::connect(address).unwrap();
        create
            .write_all(request(address, &database).as_bytes())
            .unwrap();
        let created = read_response(&mut create);
        let session = json_field(&created, "session");
        let token_a = json_field(&created, "resume_token");
        create.shutdown(Shutdown::Write).unwrap();
        let mut ignored = Vec::new();
        create.read_to_end(&mut ignored).unwrap();

        let mut websocket_a = TcpStream::connect(address).unwrap();
        let handshake_a = format!(
            "GET /orna/live/{session} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: orna.present.v1\r\nCookie: orna_session={token_a}\r\n\r\n",
            address.port()
        );
        websocket_a.write_all(handshake_a.as_bytes()).unwrap();
        let upgrade_a = read_response(&mut websocket_a);
        assert!(upgrade_a.starts_with("HTTP/1.1 101 Switching Protocols\r\n"));

        let body = format!(r#"{{"resume_token":"{token_a}","protocol":"orna.present.v1"}}"#);
        let mut resume = TcpStream::connect(address).unwrap();
        let resume_request = format!(
            "POST /orna/session/{session}/resume HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            address.port(),
            body.len()
        );
        resume.write_all(resume_request.as_bytes()).unwrap();
        let (retired_sender, retired_receiver) = std::sync::mpsc::channel();
        let retired_reader = std::thread::spawn(move || {
            websocket_a
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut retired = Vec::new();
            websocket_a.read_to_end(&mut retired).unwrap();
            retired_sender.send(()).unwrap();
        });
        retired_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let resumed = read_response(&mut resume);
        assert!(resumed.starts_with("HTTP/1.1 200 OK\r\n"));
        assert_eq!(json_field(&resumed, "session"), session);
        let token_b = json_field(&resumed, "resume_token");
        assert_ne!(token_a, token_b);
        retired_reader.join().unwrap();

        let stale_body = format!(r#"{{"resume_token":"{token_a}","protocol":"orna.present.v1"}}"#);
        let mut stale = TcpStream::connect(address).unwrap();
        let stale_request = format!(
            "POST /orna/session/{session}/resume HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{stale_body}",
            address.port(),
            stale_body.len()
        );
        stale.write_all(stale_request.as_bytes()).unwrap();
        stale.shutdown(Shutdown::Write).unwrap();
        assert!(read_response(&mut stale).starts_with("HTTP/1.1 410 Gone\r\n"));

        let mut websocket_c = TcpStream::connect(address).unwrap();
        let handshake_c = format!(
            "GET /orna/live/{session} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: orna.present.v1\r\nCookie: orna_session={token_b}\r\n\r\n",
            address.port()
        );
        websocket_c.write_all(handshake_c.as_bytes()).unwrap();
        assert!(
            read_response(&mut websocket_c).starts_with("HTTP/1.1 101 Switching Protocols\r\n")
        );
        sender.send(()).unwrap();
        let mut closed = Vec::new();
        websocket_c.read_to_end(&mut closed).unwrap();
    });

    assert_eq!(
        host.serve_until_cancellation(receiver.map(|_| ())),
        Err(LiveHostError::Cancelled)
    );
    client.join().unwrap();
    let _released = TcpListener::bind(address).unwrap();
}

#[test]
fn loopback_host_joins_an_active_application_socket_before_delete_succeeds() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    let database = initialized.metadata().database_id().to_string();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();
    let (sender, receiver) = futures::channel::oneshot::channel();
    let client = std::thread::spawn(move || {
        let mut create = TcpStream::connect(address).unwrap();
        create
            .write_all(request(address, &database).as_bytes())
            .unwrap();
        let created = read_response(&mut create);
        let session = json_field(&created, "session");
        let token = json_field(&created, "resume_token");
        create.shutdown(Shutdown::Write).unwrap();
        let mut ignored = Vec::new();
        create.read_to_end(&mut ignored).unwrap();

        let mut websocket = websocket_upgrade(address, &session, &token);
        websocket
            .write_all(&masked(
                true,
                2,
                &eval_payload(
                    uuid_bytes(&session),
                    [61; 16],
                    uuid_bytes(&database),
                    "let value: Int = 1;",
                ),
            ))
            .unwrap();
        let evaluated = websocket_frame(&mut websocket);
        assert!(matches!(
            evaluated.message,
            Message::Result {
                status: ResultStatus::RetainedWithoutValue,
                ..
            }
        ));

        let (retired_sender, retired_receiver) = std::sync::mpsc::channel();
        let retired_reader = std::thread::spawn(move || {
            websocket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut retired = Vec::new();
            websocket.read_to_end(&mut retired).unwrap();
            retired_sender.send(()).unwrap();
        });

        let mut delete = TcpStream::connect(address).unwrap();
        delete
            .write_all(delete_request(address, &session, &token).as_bytes())
            .unwrap();
        retired_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("the active socket worker must retire before DELETE replies");
        let deleted = read_response(&mut delete);
        assert!(
            deleted.starts_with("HTTP/1.1 204 No Content\r\n"),
            "{deleted}"
        );
        retired_reader.join().unwrap();

        sender.send(()).unwrap();
    });

    assert_eq!(
        host.serve_until_cancellation(receiver.map(|_| ())),
        Err(LiveHostError::Cancelled)
    );
    client.join().unwrap();
    let _released = TcpListener::bind(address).unwrap();
}

#[test]
fn loopback_host_retains_a_terminal_request_after_websocket_teardown() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    let database = initialized.metadata().database_id().to_string();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();
    let (sender, receiver) = futures::channel::oneshot::channel();
    let request_id = [7; 16];
    let request_database = database.clone();
    let client = std::thread::spawn(move || {
        let mut create = TcpStream::connect(address).unwrap();
        create
            .write_all(request(address, &request_database).as_bytes())
            .unwrap();
        let created = read_response(&mut create);
        let session = json_field(&created, "session");
        let token = json_field(&created, "resume_token");
        create.shutdown(Shutdown::Write).unwrap();
        let mut ignored = Vec::new();
        create.read_to_end(&mut ignored).unwrap();

        let mut websocket = TcpStream::connect(address).unwrap();
        let handshake = format!(
            "GET /orna/live/{session} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost:{}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: orna.present.v1\r\nCookie: orna_session={token}\r\n\r\n",
            address.port()
        );
        let mut input = handshake.into_bytes();
        input.extend(masked(
            true,
            2,
            &eval_payload(
                uuid_bytes(&session),
                request_id,
                uuid_bytes(&request_database),
                "0",
            ),
        ));
        input.extend(masked(true, 8, b""));
        websocket.write_all(&input).unwrap();
        websocket.shutdown(Shutdown::Write).unwrap();
        let mut response = Vec::new();
        websocket.read_to_end(&mut response).unwrap();
        assert!(response.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
        sender.send(()).unwrap();
        (session, json_field(&created, "runtime"))
    });

    assert_eq!(
        host.serve_until_cancellation(receiver.map(|_| ())),
        Err(LiveHostError::Cancelled)
    );
    let (session, runtime) = client.join().unwrap();
    let database_id = uuid_bytes(&database);
    assert_ne!(uuid_bytes(&runtime), [0; 16]);
    let (identity, digest) = stored_runtime_identity(database_id);
    let state = block_on(RuntimeState::open(
        initialized.repository(),
        identity,
        digest,
    ))
    .unwrap();
    let status = block_on(state.request_status_for_identity(RequestIdentity {
        session_id: uuid_bytes(&session),
        request_id,
    }))
    .unwrap()
    .unwrap();
    assert_eq!(status.state, RequestState::Completed);
    assert!(status.terminal_outcome.is_some());
    let _released = TcpListener::bind(address).unwrap();
}

#[test]
fn loopback_host_evaluates_pure_source_retains_state_and_replays_terminal_eval() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    std::fs::write(temporary.path().join("main.orna"), "use library;\n").unwrap();
    let library = temporary.path().join("library.orna");
    std::fs::write(&library, "pub fn seeded(): Int = 40;\n").unwrap();
    let database = initialized.metadata().database_id().to_string();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();
    let (sender, receiver) = futures::channel::oneshot::channel();
    let client = std::thread::spawn(move || {
        let mut create = TcpStream::connect(address).unwrap();
        create
            .write_all(request(address, &database).as_bytes())
            .unwrap();
        let created = read_response(&mut create);
        let session = json_field(&created, "session");
        let token = json_field(&created, "resume_token");
        create.shutdown(Shutdown::Write).unwrap();
        let mut ignored = Vec::new();
        create.read_to_end(&mut ignored).unwrap();

        let imported = websocket_eval(
            address,
            &session,
            &token,
            &database,
            [9; 16],
            "use library;",
        );
        assert!(matches!(
            imported.message,
            Message::Result {
                status: ResultStatus::RetainedWithoutValue,
                value: None,
                ..
            }
        ));

        let standard_import = websocket_eval(
            address,
            &session,
            &token,
            &database,
            [16; 16],
            "use std.math;",
        );
        assert!(matches!(
            standard_import.message,
            Message::Result {
                status: ResultStatus::RetainedWithoutValue,
                value: None,
                ..
            }
        ));
        let standard_value = websocket_eval(
            address,
            &session,
            &token,
            &database,
            [17; 16],
            "math.increment(41)",
        );
        let Message::Result {
            status: ResultStatus::Success,
            value: Some(standard_value),
            ..
        } = standard_value.message
        else {
            panic!("remote evaluation must use the verified standard module graph");
        };
        assert_eq!(standard_value.encode().unwrap(), vec![0x18, 42]);

        // The executable host admitted the repository project at bind time;
        // a later worktree edit must not alter this session's source graph.
        std::fs::write(&library, "pub fn seeded(): Int = 41;\n").unwrap();

        let seeded = websocket_eval(
            address,
            &session,
            &token,
            &database,
            [10; 16],
            "library.seeded() + 2",
        );
        let Message::Result {
            status: ResultStatus::Success,
            value: Some(seeded_value),
            ..
        } = seeded.message
        else {
            panic!("{seeded:?}");
        };
        assert_eq!(seeded_value.encode().unwrap(), vec![0x18, 42]);

        let declared = websocket_eval(
            address,
            &session,
            &token,
            &database,
            [11; 16],
            "let answer: Int = 40;",
        );
        assert!(matches!(
            declared.message,
            Message::Result {
                status: ResultStatus::RetainedWithoutValue,
                value: None,
                ..
            }
        ));
        let forty_two =
            websocket_eval(address, &session, &token, &database, [12; 16], "answer + 2");
        let Message::Result {
            status: ResultStatus::Success,
            value: Some(forty_two),
            ..
        } = forty_two.message
        else {
            panic!("pure evaluation must return a typed result");
        };
        let forty_two = forty_two.encode().unwrap();

        let forty_three = websocket_eval(address, &session, &token, &database, [13; 16], "$_ + 1");
        let Message::Result {
            status: ResultStatus::Success,
            value: Some(forty_three),
            ..
        } = forty_three.message
        else {
            panic!("successful stateful evaluation must return a typed result");
        };
        let forty_three = forty_three.encode().unwrap();
        assert_ne!(forty_two, forty_three);

        let rejected = websocket_eval(
            address,
            &session,
            &token,
            &database,
            [14; 16],
            "std.net.http.get(\"https://example.com\")",
        );
        assert!(matches!(
            rejected.message,
            Message::Result {
                status: ResultStatus::Failure,
                value: None,
                diagnostic: Some(_),
                ..
            }
        ));

        let table_write = websocket_eval(
            address,
            &session,
            &token,
            &database,
            [18; 16],
            "table Note(id: Int) { value: Int, } fn main() { Note.insert({ id: 1, value: 2 }); }",
        );
        assert!(matches!(
            table_write.message,
            Message::Result {
                status: ResultStatus::Failure,
                value: None,
                diagnostic: Some(_),
                ..
            }
        ));
        let after_rejection = websocket_eval(address, &session, &token, &database, [19; 16], "$_");
        let Message::Result {
            status: ResultStatus::Success,
            value: Some(after_rejection),
            ..
        } = after_rejection.message
        else {
            panic!("a rejected effect must not replace the retained pure result");
        };
        assert_eq!(after_rejection.encode().unwrap(), forty_three);

        let replayed = websocket_eval(address, &session, &token, &database, [13; 16], "$_ + 1");
        let current = websocket_eval(address, &session, &token, &database, [15; 16], "$_");
        for response in [replayed, current] {
            let Message::Result {
                status: ResultStatus::Success,
                value: Some(value),
                ..
            } = response.message
            else {
                panic!("terminal Eval must replay a successful typed result");
            };
            assert_eq!(value.encode().unwrap(), forty_three);
        }
        sender.send(()).unwrap();
    });

    assert_eq!(
        host.serve_until_cancellation(receiver.map(|_| ())),
        Err(LiveHostError::Cancelled)
    );
    client.join().unwrap();
    let _released = TcpListener::bind(address).unwrap();
}

#[test]
fn loopback_host_replays_pre_overlay_eval_rejection_without_binding_source() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    let database = initialized.metadata().database_id().to_string();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();
    let (sender, receiver) = futures::channel::oneshot::channel();
    let client = std::thread::spawn(move || {
        let mut create = TcpStream::connect(address).unwrap();
        create
            .write_all(request(address, &database).as_bytes())
            .unwrap();
        let created = read_response(&mut create);
        let session = json_field(&created, "session");
        let token = json_field(&created, "resume_token");
        create.shutdown(Shutdown::Write).unwrap();
        let mut ignored = Vec::new();
        create.read_to_end(&mut ignored).unwrap();

        let stale =
            orna_foundation_v1::CanonicalSnapshot::cwd(uuid_bytes(&database), [0x77; 16], 1.into())
                .unwrap();
        let request_id = [61; 16];
        let first = websocket_eval_at(
            address,
            &session,
            &token,
            &database,
            request_id,
            Some(stale.clone()),
            "let hidden: Int = 1;",
        );
        assert!(matches!(
            &first.message,
            Message::Result {
                status: ResultStatus::Failure,
                diagnostic: Some(_),
                ..
            }
        ));

        let replay = websocket_eval_at(
            address,
            &session,
            &token,
            &database,
            request_id,
            Some(stale),
            "let hidden: Int = 1;",
        );
        assert_eq!(replay, first);

        let hidden = websocket_eval(address, &session, &token, &database, [62; 16], "hidden");
        assert!(matches!(
            &hidden.message,
            Message::Result {
                status: ResultStatus::Failure,
                diagnostic: Some(_),
                ..
            }
        ));
        sender.send(()).unwrap();
    });

    assert_eq!(
        host.serve_until_cancellation(receiver.map(|_| ())),
        Err(LiveHostError::Cancelled)
    );
    client.join().unwrap();
    let _released = TcpListener::bind(address).unwrap();
}

#[test]
fn loopback_host_rejects_unsupported_watch() {
    let temporary = TemporaryRepository::new();
    let initialized = initialize_repository(temporary.path()).unwrap();
    let database = initialized.metadata().database_id().to_string();
    let host = LiveOnceHost::bind(initialized.repository(), 0).unwrap();
    let address = host.address();
    let (sender, receiver) = futures::channel::oneshot::channel();
    let client = std::thread::spawn(move || {
        let mut create = TcpStream::connect(address).unwrap();
        create
            .write_all(request(address, &database).as_bytes())
            .unwrap();
        let created = read_response(&mut create);
        let session = json_field(&created, "session");
        let token = json_field(&created, "resume_token");
        create.shutdown(Shutdown::Write).unwrap();
        let mut ignored = Vec::new();
        create.read_to_end(&mut ignored).unwrap();

        let request = [21; 16];
        let response = websocket_watch(address, &session, &token, &database, request, "1 + 1");
        assert_eq!(response.request, Some(request));
        let Message::Diagnostic { recoverable, .. } = response.message else {
            panic!("unsupported Watch must produce a correlated diagnostic");
        };
        assert_eq!(recoverable, None);
        assert_eq!(response.watch, None);
        sender.send(()).unwrap();
    });

    assert_eq!(
        host.serve_until_cancellation(receiver.map(|_| ())),
        Err(LiveHostError::Cancelled)
    );
    client.join().unwrap();
    let _released = TcpListener::bind(address).unwrap();
}
