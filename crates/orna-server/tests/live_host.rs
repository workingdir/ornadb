use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures::FutureExt;
use orna_repository_v1::initialize_repository;
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

fn request(database: &str) -> String {
    let body = format!(r#"{{"database":"{database}","protocol":"orna.present.v1"}}"#);
    format!(
        "POST /orna/session HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
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
            .write_all(request(&request_database).as_bytes())
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
            .write_all(request("00000000-0000-0000-0000-000000000001").as_bytes())
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
            .write_all(request(&request_database).as_bytes())
            .unwrap();
        let created = read_response(&mut client);
        assert!(created.starts_with("HTTP/1.1 201 Created\r\n"));
        let session = json_field(&created, "session");
        let first_token = json_field(&created, "resume_token");

        let body = format!(r#"{{"resume_token":"{first_token}","protocol":"orna.present.v1"}}"#);
        let resume = format!(
            "POST /orna/session/{session}/resume HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        client.write_all(resume.as_bytes()).unwrap();
        let resumed = read_response(&mut client);
        assert!(resumed.starts_with("HTTP/1.1 200 OK\r\n"));
        assert_eq!(json_field(&resumed, "session"), session);
        let second_token = json_field(&resumed, "resume_token");
        assert_ne!(second_token, first_token);

        let delete = format!(
            "DELETE /orna/session/{session} HTTP/1.1\r\nHost: localhost\r\nOrigin: http://localhost\r\nAuthorization: Bearer {second_token}\r\nContent-Length: 0\r\n\r\n"
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
            .write_all(request(&request_database).as_bytes())
            .unwrap();
        first.shutdown(Shutdown::Write).unwrap();
        let mut first_response = Vec::new();
        first.read_to_end(&mut first_response).unwrap();
        assert!(first_response.starts_with(b"HTTP/1.1 201 Created\r\n"));

        let mut second = TcpStream::connect(address).unwrap();
        second
            .write_all(request("00000000-0000-0000-0000-000000000001").as_bytes())
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
