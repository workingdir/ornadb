use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::{FutureExt, executor::block_on};
use orna_protocol_v1::{
    DatabaseContext, Envelope, Limits as ProtocolLimits, Message, PresentationContext,
    canonical_request_fingerprint,
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

fn eval_payload(session: [u8; 16], request: [u8; 16]) -> Vec<u8> {
    let mut envelope = Envelope {
        request: Some(request),
        watch: None,
        message: Message::Eval {
            source: "0".into(),
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
            &eval_payload(uuid_bytes(&session), request_id),
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
    let repository_id = uuid_bytes(&runtime);
    let identity = RuntimeIdentity {
        database_id,
        repository_id,
    };
    let mut digest = [0; 32];
    digest[..16].copy_from_slice(&database_id);
    digest[16..].copy_from_slice(&repository_id);
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
