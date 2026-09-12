use futures_util::{SinkExt, StreamExt};
use orna_client::{
    AuthenticatedWebSocketTransport, LiveByteDriver, LiveClientConfig, LiveTransportError,
    TlsPolicy,
};
use orna_protocol_v1::{Envelope, Limits, Message as ProtocolMessage, ResultStatus};
use reqwest::Url;
use std::{collections::BTreeMap, future::Future, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{Role, WebSocketConfig, frame::coding::CloseCode},
    },
};

fn config(endpoint: &str, policy: TlsPolicy) -> LiveClientConfig {
    LiveClientConfig {
        endpoint: Url::parse(endpoint).unwrap(),
        origin: "http://localhost".into(),
        limits: Limits::default(),
        request_timeout: Duration::from_secs(5),
        tls_policy: policy,
    }
}

#[test]
fn plaintext_is_only_allowed_for_explicit_trusted_loopback() {
    assert!(
        config("http://localhost:8080", TlsPolicy::TrustedLoopbackOnly)
            .validate()
            .is_ok()
    );
    assert!(
        config("http://127.0.0.1:8080", TlsPolicy::TrustedLoopbackOnly)
            .validate()
            .is_ok()
    );
    assert!(
        config("http://example.test", TlsPolicy::TrustedLoopbackOnly)
            .validate()
            .is_err()
    );
    assert!(
        config("http://localhost:8080", TlsPolicy::RequireTls)
            .validate()
            .is_err()
    );
}

#[test]
fn secure_endpoint_is_accepted_and_other_schemes_are_rejected() {
    assert!(
        config("https://example.test", TlsPolicy::RequireTls)
            .validate()
            .is_ok()
    );
    assert!(
        config("ftp://localhost", TlsPolicy::TrustedLoopbackOnly)
            .validate()
            .is_err()
    );
}

fn block_on<F: Future>(future: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime
        .block_on(async move { tokio::time::timeout(Duration::from_secs(1), future).await })
        .expect("fake WebSocket operation timed out")
}

fn host_result() -> Vec<u8> {
    Envelope {
        request: Some([3; 16]),
        watch: None,
        message: ProtocolMessage::Result {
            status: ResultStatus::Failure,
            value: None,
            fingerprint: [4; 32],
            diagnostic: None,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default())
    .unwrap()
}

fn wrong_direction() -> Vec<u8> {
    Envelope {
        request: Some([5; 16]),
        watch: Some([6; 16]),
        message: ProtocolMessage::Resync,
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default())
    .unwrap()
}

fn close_code(message: Message) -> CloseCode {
    let Message::Close(Some(frame)) = message else {
        panic!("expected a close frame");
    };
    frame.code
}

#[test]
fn local_fake_wire_preserves_ping_pong_and_normal_close() {
    let (client_io, server_io) = duplex(4096);
    let client = block_on(WebSocketStream::from_raw_socket(
        client_io,
        Role::Client,
        None,
    ));
    let mut server = block_on(WebSocketStream::from_raw_socket(
        server_io,
        Role::Server,
        None,
    ));
    let mut transport =
        AuthenticatedWebSocketTransport::from_authenticated_socket(client, Limits::default())
            .unwrap();

    block_on(server.send(Message::Ping(vec![1].into()))).unwrap();
    let result = host_result();
    block_on(server.send(Message::Binary(result.clone().into()))).unwrap();
    assert_eq!(
        block_on(transport.receive_binary(Limits::default().max_message_bytes)).unwrap(),
        result
    );
    assert_eq!(
        block_on(server.next()).unwrap().unwrap(),
        Message::Pong(vec![1].into())
    );

    block_on(transport.send_binary(vec![7, 8])).unwrap();
    assert_eq!(
        block_on(server.next()).unwrap().unwrap(),
        Message::Binary(vec![7, 8].into())
    );

    block_on(server.send(Message::Close(None))).unwrap();
    assert!(matches!(
        block_on(transport.receive_binary(Limits::default().max_message_bytes)),
        Err(LiveTransportError::WebSocket(
            tokio_tungstenite::tungstenite::Error::ConnectionClosed
        ))
    ));

    let (raw_client_io, mut raw_server_io) = duplex(4096);
    let raw_client = block_on(WebSocketStream::from_raw_socket(
        raw_client_io,
        Role::Client,
        None,
    ));
    let mut raw_transport =
        AuthenticatedWebSocketTransport::from_authenticated_socket(raw_client, Limits::default())
            .unwrap();
    block_on(raw_transport.send_binary(vec![9, 10])).unwrap();
    let mut header = [0; 8];
    block_on(raw_server_io.read_exact(&mut header)).unwrap();
    assert_eq!(header[0], 0x82);
    assert_ne!(
        header[1] & 0x80,
        0,
        "client WebSocket frames must be masked"
    );
    assert_eq!(header[1] & 0x7f, 2);
    let expected = [9, 10];
    let unmasked = [header[6] ^ header[2], header[7] ^ header[3]];
    assert_eq!(unmasked, expected);
}

#[test]
fn protocol_violations_close_without_consuming_or_resyncing() {
    let (client_io, server_io) = duplex(4096);
    let client = block_on(WebSocketStream::from_raw_socket(
        client_io,
        Role::Client,
        None,
    ));
    let mut server = block_on(WebSocketStream::from_raw_socket(
        server_io,
        Role::Server,
        None,
    ));
    let mut transport =
        AuthenticatedWebSocketTransport::from_authenticated_socket(client, Limits::default())
            .unwrap();

    block_on(server.send(Message::Binary(vec![0xff].into()))).unwrap();
    block_on(server.send(Message::Ping(vec![9].into()))).unwrap();
    assert!(matches!(
        block_on(transport.receive_binary(Limits::default().max_message_bytes)),
        Err(LiveTransportError::Protocol(_))
    ));
    assert_eq!(
        close_code(block_on(server.next()).unwrap().unwrap()),
        CloseCode::Protocol
    );
    assert!(matches!(
        block_on(transport.receive_binary(Limits::default().max_message_bytes)),
        Err(LiveTransportError::WebSocket(
            tokio_tungstenite::tungstenite::Error::ConnectionClosed
        ))
    ));
    assert!(matches!(
        block_on(transport.send_binary(vec![1])),
        Err(LiveTransportError::WebSocket(
            tokio_tungstenite::tungstenite::Error::ConnectionClosed
        ))
    ));
    match block_on(async { tokio::time::timeout(Duration::from_millis(20), server.next()).await }) {
        Ok(Some(Ok(Message::Pong(_) | Message::Binary(_)))) => {
            panic!("terminal transport must not consume queued input or send a resync")
        }
        Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) | Err(_) => {}
        Ok(Some(Ok(message))) => panic!("unexpected post-close WebSocket message: {message:?}"),
    }

    let (client_io, server_io) = duplex(4096);
    let client = block_on(WebSocketStream::from_raw_socket(
        client_io,
        Role::Client,
        None,
    ));
    let mut server = block_on(WebSocketStream::from_raw_socket(
        server_io,
        Role::Server,
        None,
    ));
    let mut transport =
        AuthenticatedWebSocketTransport::from_authenticated_socket(client, Limits::default())
            .unwrap();

    block_on(server.send(Message::Binary(wrong_direction().into()))).unwrap();
    assert!(matches!(
        block_on(transport.receive_binary(Limits::default().max_message_bytes)),
        Err(LiveTransportError::Protocol(
            orna_protocol_v1::Error::InvalidMessage
        ))
    ));
    assert_eq!(
        close_code(block_on(server.next()).unwrap().unwrap()),
        CloseCode::Protocol
    );

    let (client_io, server_io) = duplex(4096);
    let client = block_on(WebSocketStream::from_raw_socket(
        client_io,
        Role::Client,
        None,
    ));
    let mut server = block_on(WebSocketStream::from_raw_socket(
        server_io,
        Role::Server,
        None,
    ));
    let mut transport =
        AuthenticatedWebSocketTransport::from_authenticated_socket(client, Limits::default())
            .unwrap();

    block_on(server.send(Message::Text("text".into()))).unwrap();
    assert!(matches!(
        block_on(transport.receive_binary(Limits::default().max_message_bytes)),
        Err(LiveTransportError::Response(
            "text WebSocket message rejected"
        ))
    ));
    assert_eq!(
        close_code(block_on(server.next()).unwrap().unwrap()),
        CloseCode::Unsupported
    );
}

#[test]
fn fragmented_oversize_input_closes_with_1009() {
    let mut small = Limits::default();
    small.max_message_bytes = 1;
    let (client_io, mut server_io) = duplex(4096);
    let client = block_on(WebSocketStream::from_raw_socket(
        client_io,
        Role::Client,
        Some(
            WebSocketConfig::default()
                .max_message_size(Some(small.max_message_bytes))
                .max_frame_size(Some(small.max_message_bytes)),
        ),
    ));
    let mut transport =
        AuthenticatedWebSocketTransport::from_authenticated_socket(client, small).unwrap();

    block_on(server_io.write_all(&[0x02, 0x01, 1, 0x80, 0x01, 2])).unwrap();
    assert!(matches!(
        block_on(transport.receive_binary(small.max_message_bytes)),
        Err(LiveTransportError::Protocol(orna_protocol_v1::Error::Limit))
    ));
    let mut header = [0; 8];
    block_on(server_io.read_exact(&mut header)).unwrap();
    assert_eq!(header[0], 0x88);
    assert_ne!(header[1] & 0x80, 0);
    assert_eq!(header[1] & 0x7f, 2);
    let code = u16::from_be_bytes([header[6] ^ header[2], header[7] ^ header[3]]);
    assert_eq!(code, 1009);
    assert!(matches!(
        block_on(transport.receive_binary(small.max_message_bytes)),
        Err(LiveTransportError::WebSocket(
            tokio_tungstenite::tungstenite::Error::ConnectionClosed
        ))
    ));
}
