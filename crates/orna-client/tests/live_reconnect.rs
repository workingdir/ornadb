use futures_util::{SinkExt, StreamExt};
use orna_client::{
    AuthenticatedWebSocketTransport, BootstrappedLiveAttachment, LiveClient, LiveClientConfig,
    LiveReconnectError, LiveSessionDriver, LiveSessionEvent, PrefetchedBinaryTransport,
    PresentRenderer, RequestIdAllocator, TlsPolicy,
};
use orna_protocol_v1::{
    CanonicalSnapshot, Envelope, Limits, Message, PresentNode, PresentationContext,
};
use reqwest::Url;
use std::{collections::BTreeMap, future::Future, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::{
    accept_hdr_async,
    tungstenite::{
        Message as WebSocketMessage,
        handshake::server::{
            ErrorResponse as WebSocketErrorResponse, Request as WebSocketRequest,
            Response as WebSocketResponse,
        },
        http::StatusCode,
    },
};

const SESSION: &str = "00000000-0000-0000-0000-000000000001";
const DATABASE: &str = "00000000-0000-0000-0000-000000000002";
const RUNTIME: &str = "00000000-0000-0000-0000-000000000003";
const ROTATED_RUNTIME: &str = "00000000-0000-0000-0000-000000000004";
const FIRST_TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
const SECOND_TOKEN: &str = "EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE";
const THIRD_TOKEN: &str = "IIIIIIIIIIIIIIIIIIIIIIIIIIIIIIIIIIIIIIIIIII";

#[derive(Clone, Copy)]
enum ReplacementResponse {
    Resubscribe,
    ExistingAutomatic,
    WrongWatch,
    Correlated,
    RuntimeChanged,
    LimitsChanged,
    TransportFailure,
}

#[derive(Default)]
struct Renderer {
    watches: Vec<[u8; 16]>,
    revisions: Vec<u64>,
}

impl PresentRenderer for Renderer {
    type Error = &'static str;

    fn publish(
        &mut self,
        watch: [u8; 16],
        revision: u64,
        _: &CanonicalSnapshot,
        _: &PresentNode,
    ) -> Result<(), Self::Error> {
        self.watches.push(watch);
        self.revisions.push(revision);
        Ok(())
    }
}

#[derive(Default)]
struct Requests;

impl RequestIdAllocator for Requests {
    fn next_request_id(&mut self) -> [u8; 16] {
        [9; 16]
    }
}

type Driver = LiveSessionDriver<
    PrefetchedBinaryTransport<AuthenticatedWebSocketTransport<MaybeTlsStream<TcpStream>>>,
    Renderer,
    Requests,
>;

fn run<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async move {
            tokio::time::timeout(Duration::from_secs(5), future)
                .await
                .expect("loopback live reconnect test timed out")
        })
}

fn subscribe() -> Envelope {
    Envelope {
        request: Some([1; 16]),
        watch: None,
        message: Message::Subscribe {
            resource: [2; 16],
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: Some(80),
                theme: "terminal/default".into(),
                supported_kinds: vec!["text".into()],
            },
        },
        extensions: BTreeMap::new(),
    }
}

fn snapshot_with_request(
    watch: [u8; 16],
    revision: u8,
    text: &str,
    request: Option<[u8; 16]>,
) -> Vec<u8> {
    let mut bytes = vec![0xa5, 0x00, 0x01, 0x01, 16, 0x02];
    match request {
        Some(request) => {
            bytes.push(0x50);
            bytes.extend(request);
        }
        None => bytes.push(0xf6),
    }
    bytes.extend([0x03, 0x50]);
    bytes.extend(watch);
    bytes.extend([0x04, 0xa3, 0x00, revision, 0x01]);
    bytes.extend([0xd9, 0xea, 0x6c, 0x84, 0x64]);
    bytes.extend(b"text");
    bytes.extend([0xf6, 0xa1, 0x64]);
    bytes.extend(b"text");
    bytes.push(0x60 + text.len() as u8);
    bytes.extend(text.as_bytes());
    bytes.extend([0x80, 0x02, 0x84, 0x01, 0xd8, 0x25, 0x50]);
    bytes.extend([1; 16]);
    bytes.extend([0x66]);
    bytes.extend(b"sha256");
    bytes.extend([0x58, 32]);
    bytes.extend([2; 32]);
    Envelope::decode(&bytes, Limits::default())
        .unwrap()
        .encode(Limits::default())
        .unwrap()
}

fn snapshot(watch: [u8; 16], revision: u8, text: &str) -> Vec<u8> {
    snapshot_with_request(watch, revision, text, Some([1; 16]))
}

fn automatic_snapshot(watch: [u8; 16], revision: u8, text: &str) -> Vec<u8> {
    snapshot_with_request(watch, revision, text, None)
}

fn session_body(token: &str, runtime: &str, limits: &str) -> String {
    format!(
        r#"{{"session":"{SESSION}","database":"{DATABASE}","runtime":"{runtime}","resume_token":"{token}","websocket_path":"/orna/live/{SESSION}","lease_ms":30000,"limits":{limits}}}"#,
    )
}

fn default_limits() -> &'static str {
    r#"{"max_message_bytes":16777216,"max_depth":64,"max_nodes":100000,"max_collection_items":100000,"max_outgoing_bytes":16777216,"request_retention_ms":30000}"#
}

fn changed_limits() -> &'static str {
    r#"{"max_message_bytes":16777216,"max_depth":64,"max_nodes":100000,"max_collection_items":99999,"max_outgoing_bytes":16777216,"request_retention_ms":30000}"#
}

async fn read_http(stream: &mut TcpStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut byte = [0; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).await.unwrap();
        bytes.push(byte[0]);
    }
    let headers = std::str::from_utf8(&bytes).unwrap();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let mut body = vec![0; content_length];
    stream.read_exact(&mut body).await.unwrap();
    bytes.extend(body);
    bytes
}

async fn respond(stream: &mut TcpStream, status: &str, token: &str, runtime: &str, limits: &str) {
    let body = session_body(token, runtime, limits);
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nSet-Cookie: orna_session=opaque-{token}; Path=/orna/live/{SESSION}; HttpOnly; SameSite=Strict\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
}

async fn websocket_snapshot(
    stream: TcpStream,
    response: ReplacementResponse,
    token: &str,
    watch: [u8; 16],
) -> Option<tokio_tungstenite::WebSocketStream<TcpStream>> {
    let expected_path = format!("/orna/live/{SESSION}");
    let expected_cookie = format!("orna_session=opaque-{token}");
    let mut socket = accept_hdr_async(
        stream,
        |request: &WebSocketRequest, mut response: WebSocketResponse| {
            let admitted = request.uri().path() == expected_path
                && request
                    .headers()
                    .get("cookie")
                    .and_then(|value| value.to_str().ok())
                    == Some(expected_cookie.as_str())
                && request
                    .headers()
                    .get("sec-websocket-protocol")
                    .and_then(|value| value.to_str().ok())
                    == Some("orna.present.v1");
            if !admitted {
                let mut rejection =
                    WebSocketErrorResponse::new(Some("live attachment rejected".into()));
                *rejection.status_mut() = StatusCode::FORBIDDEN;
                return Err(rejection);
            }
            response
                .headers_mut()
                .insert("sec-websocket-protocol", "orna.present.v1".parse().unwrap());
            Ok(response)
        },
    )
    .await
    .unwrap();
    if matches!(response, ReplacementResponse::Resubscribe) {
        let Some(Ok(WebSocketMessage::Binary(request))) = socket.next().await else {
            panic!("expected subscribe binary frame");
        };
        assert_eq!(
            Envelope::decode(&request, Limits::default()).unwrap(),
            subscribe()
        );
        let revision = if token == FIRST_TOKEN { 0 } else { 1 };
        let text = if token == FIRST_TOKEN { "old" } else { "new" };
        socket
            .send(WebSocketMessage::Binary(
                snapshot(watch, revision, text).into(),
            ))
            .await
            .unwrap();
    } else {
        let frame = match response {
            ReplacementResponse::ExistingAutomatic => automatic_snapshot([7; 16], 1, "new"),
            ReplacementResponse::WrongWatch => automatic_snapshot([8; 16], 1, "wrong-watch"),
            ReplacementResponse::Correlated => snapshot([7; 16], 1, "correlated"),
            ReplacementResponse::RuntimeChanged
            | ReplacementResponse::LimitsChanged
            | ReplacementResponse::TransportFailure
            | ReplacementResponse::Resubscribe => unreachable!(),
        };
        socket
            .send(WebSocketMessage::Binary(frame.into()))
            .await
            .unwrap();
        if let Ok(Some(Ok(WebSocketMessage::Binary(_)))) =
            tokio::time::timeout(Duration::from_millis(100), socket.next()).await
        {
            panic!("existing-watch resume must not receive a Subscribe frame");
        }
    }
    (token == FIRST_TOKEN).then_some(socket)
}

async fn spawn_server(
    response: ReplacementResponse,
    runtime: &'static str,
    limits: &'static str,
) -> (Url, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut create, _) = listener.accept().await.unwrap();
        assert!(
            String::from_utf8(read_http(&mut create).await)
                .unwrap()
                .starts_with("POST /orna/session ")
        );
        respond(
            &mut create,
            "201 Created",
            FIRST_TOKEN,
            RUNTIME,
            default_limits(),
        )
        .await;

        let (old_socket, _) = listener.accept().await.unwrap();
        let old_socket = websocket_snapshot(
            old_socket,
            ReplacementResponse::Resubscribe,
            FIRST_TOKEN,
            [7; 16],
        )
        .await
        .expect("initial attachment socket");

        let (mut resume, _) = listener.accept().await.unwrap();
        let resume_request = String::from_utf8(read_http(&mut resume).await).unwrap();
        assert!(resume_request.contains(FIRST_TOKEN));
        respond(&mut resume, "200 OK", SECOND_TOKEN, runtime, limits).await;
        drop(old_socket);

        if matches!(
            response,
            ReplacementResponse::RuntimeChanged | ReplacementResponse::LimitsChanged
        ) {
            assert!(
                tokio::time::timeout(Duration::from_millis(250), listener.accept())
                    .await
                    .is_err(),
                "mismatched resume must not open a replacement attachment"
            );
            return;
        }

        let (replacement, _) = listener.accept().await.unwrap();
        if matches!(response, ReplacementResponse::TransportFailure) {
            drop(replacement);
            let (mut retry, _) = listener.accept().await.unwrap();
            let retry_request = String::from_utf8(read_http(&mut retry).await).unwrap();
            assert!(retry_request.contains(SECOND_TOKEN));
            respond(&mut retry, "200 OK", THIRD_TOKEN, runtime, limits).await;
        } else {
            let _ = websocket_snapshot(replacement, response, SECOND_TOKEN, [8; 16]).await;
        }
    });
    (Url::parse(&format!("http://{address}")).unwrap(), task)
}

fn client(endpoint: Url) -> LiveClient {
    LiveClient::new(LiveClientConfig {
        endpoint,
        origin: "http://localhost".into(),
        limits: Limits::default(),
        request_timeout: Duration::from_secs(2),
        tls_policy: TlsPolicy::TrustedLoopbackOnly,
    })
    .unwrap()
}

async fn initial_driver(client: &LiveClient) -> (orna_client::LiveSession, Driver) {
    let session = client.create_session([2; 16]).await.unwrap();
    let transport = client.connect(&session).await.unwrap();
    let attachment = BootstrappedLiveAttachment::start(transport, subscribe(), session.limits())
        .await
        .unwrap();
    (
        session,
        attachment
            .into_driver(Renderer::default(), Requests)
            .unwrap(),
    )
}

#[test]
fn reconnect_resubscribes_with_a_new_watch() {
    run(async {
        let (endpoint, server) =
            spawn_server(ReplacementResponse::Resubscribe, RUNTIME, default_limits()).await;
        let client = client(endpoint);
        let (session, mut driver) = initial_driver(&client).await;
        assert!(matches!(
            driver.receive_once().await,
            Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
        ));
        let rotated = match client
            .reconnect_driver(&session, &mut driver, subscribe())
            .await
        {
            Ok(session) => session,
            Err(_) => panic!("resubscribe reconnect unexpectedly failed"),
        };
        assert_eq!(rotated.session_id(), session.session_id());
        assert_eq!(driver.watch(), [8; 16]);
        assert!(matches!(
            driver.receive_once().await,
            Ok(LiveSessionEvent::SnapshotPublished { revision: 1 })
        ));
        server.await.unwrap();
    });
}

#[test]
fn resume_existing_watch_uses_automatic_snapshot_without_subscribe() {
    run(async {
        let (endpoint, server) = spawn_server(
            ReplacementResponse::ExistingAutomatic,
            RUNTIME,
            default_limits(),
        )
        .await;
        let client = client(endpoint);
        let (session, mut driver) = initial_driver(&client).await;
        assert!(matches!(
            driver.receive_once().await,
            Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
        ));
        let rotated = match client.resume_driver(&session, &mut driver).await {
            Ok(session) => session,
            Err(_) => panic!("existing-watch resume unexpectedly failed"),
        };
        assert_eq!(rotated.session_id(), session.session_id());
        assert_eq!(driver.watch(), [7; 16]);
        assert!(driver.presentation().awaiting_snapshot());
        assert_eq!(driver.presentation().published().unwrap().revision(), 0);
        assert!(matches!(
            driver.receive_once().await,
            Ok(LiveSessionEvent::SnapshotPublished { revision: 1 })
        ));
        server.await.unwrap();
    });
}

#[test]
fn existing_watch_resume_rejects_wrong_watch_without_mutation() {
    run(async {
        let (endpoint, server) =
            spawn_server(ReplacementResponse::WrongWatch, RUNTIME, default_limits()).await;
        let client = client(endpoint);
        let (session, mut driver) = initial_driver(&client).await;
        assert!(matches!(
            driver.receive_once().await,
            Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
        ));
        assert!(matches!(
            client.resume_driver(&session, &mut driver).await,
            Err(LiveReconnectError::AfterResume(_))
        ));
        assert_eq!(driver.watch(), [7; 16]);
        assert_eq!(driver.presentation().published().unwrap().revision(), 0);
        server.await.unwrap();
    });
}

#[test]
fn existing_watch_resume_rejects_correlated_snapshot_without_mutation() {
    run(async {
        let (endpoint, server) =
            spawn_server(ReplacementResponse::Correlated, RUNTIME, default_limits()).await;
        let client = client(endpoint);
        let (session, mut driver) = initial_driver(&client).await;
        assert!(matches!(
            driver.receive_once().await,
            Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
        ));
        assert!(matches!(
            client.resume_driver(&session, &mut driver).await,
            Err(LiveReconnectError::AfterResume(_))
        ));
        assert_eq!(driver.watch(), [7; 16]);
        assert_eq!(driver.presentation().published().unwrap().revision(), 0);
        server.await.unwrap();
    });
}

#[test]
fn existing_watch_resume_rejects_runtime_generation_change_before_socket() {
    run(async {
        let (endpoint, server) = spawn_server(
            ReplacementResponse::RuntimeChanged,
            ROTATED_RUNTIME,
            default_limits(),
        )
        .await;
        let client = client(endpoint);
        let (session, mut driver) = initial_driver(&client).await;
        assert!(matches!(
            driver.receive_once().await,
            Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
        ));
        assert!(matches!(
            client.resume_driver(&session, &mut driver).await,
            Err(LiveReconnectError::AfterResume(_))
        ));
        assert_eq!(driver.watch(), [7; 16]);
        assert_eq!(driver.presentation().published().unwrap().revision(), 0);
        server.await.unwrap();
    });
}

#[test]
fn existing_watch_resume_rejects_limit_change_before_socket() {
    run(async {
        let (endpoint, server) = spawn_server(
            ReplacementResponse::LimitsChanged,
            RUNTIME,
            changed_limits(),
        )
        .await;
        let client = client(endpoint);
        let (session, mut driver) = initial_driver(&client).await;
        assert!(matches!(
            driver.receive_once().await,
            Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
        ));
        assert!(matches!(
            client.resume_driver(&session, &mut driver).await,
            Err(LiveReconnectError::AfterResume(_))
        ));
        assert_eq!(driver.watch(), [7; 16]);
        assert_eq!(driver.presentation().published().unwrap().revision(), 0);
        server.await.unwrap();
    });
}

#[test]
fn reconnect_resubscribe_rejects_runtime_generation_change_before_socket() {
    run(async {
        let (endpoint, server) = spawn_server(
            ReplacementResponse::RuntimeChanged,
            ROTATED_RUNTIME,
            default_limits(),
        )
        .await;
        let client = client(endpoint);
        let (session, mut driver) = initial_driver(&client).await;
        assert!(matches!(
            driver.receive_once().await,
            Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
        ));
        assert!(matches!(
            client
                .reconnect_driver(&session, &mut driver, subscribe())
                .await,
            Err(LiveReconnectError::AfterResume(_))
        ));
        assert_eq!(driver.watch(), [7; 16]);
        assert_eq!(driver.presentation().published().unwrap().revision(), 0);
        server.await.unwrap();
    });
}

#[test]
fn reconnect_failure_returns_rotated_retry_session_without_mutating_visible_driver() {
    run(async {
        let (endpoint, server) = spawn_server(
            ReplacementResponse::TransportFailure,
            RUNTIME,
            default_limits(),
        )
        .await;
        let client = client(endpoint);
        let (session, mut driver) = initial_driver(&client).await;
        assert!(matches!(
            driver.receive_once().await,
            Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
        ));

        let error = match client
            .reconnect_driver(&session, &mut driver, subscribe())
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("replacement connection unexpectedly succeeded"),
        };
        let retry = match error {
            LiveReconnectError::AfterResume(failure) => failure.into_session(),
            LiveReconnectError::Resume(_) => panic!("resume completed before replacement failure"),
        };
        assert_eq!(retry.session_id(), session.session_id());
        assert_eq!(driver.watch(), [7; 16]);
        assert_eq!(driver.presentation().published().unwrap().revision(), 0);
        let rotated_again = client.resume_session(&retry).await.unwrap();
        assert_eq!(rotated_again.session_id(), session.session_id());
        server.await.unwrap();
    });
}
