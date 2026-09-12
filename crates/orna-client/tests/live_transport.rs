use futures_util::{SinkExt, StreamExt};
use orna_client::{
    AuthenticatedWebSocketTransport, LiveByteDriver, LiveClientConfig, LiveTransportError,
    TlsPolicy,
};
use orna_protocol_v1::Limits;
use reqwest::Url;
use std::{future::Future, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
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

#[test]
fn local_fake_wire_handles_text_control_masking_and_bounds() {
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

    block_on(server.send(Message::Ping(vec![1].into()))).unwrap();
    block_on(server.send(Message::Binary(vec![3].into()))).unwrap();
    assert_eq!(
        block_on(transport.receive_binary(Limits::default().max_message_bytes)).unwrap(),
        vec![3]
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

    let mut small = Limits::default();
    small.max_message_bytes = 1;
    let (small_client_io, mut small_server_io) = duplex(4096);
    let small_client = block_on(WebSocketStream::from_raw_socket(
        small_client_io,
        Role::Client,
        None,
    ));
    let mut small_transport =
        AuthenticatedWebSocketTransport::from_authenticated_socket(small_client, small).unwrap();
    block_on(small_server_io.write_all(&[0x02, 0x01, 1, 0x80, 0x01, 2])).unwrap();
    assert!(matches!(
        block_on(small_transport.receive_binary(1)),
        Err(LiveTransportError::Protocol(orna_protocol_v1::Error::Limit))
    ));

    let (fragment_client_io, mut fragment_server_io) = duplex(4096);
    let fragment_client = block_on(WebSocketStream::from_raw_socket(
        fragment_client_io,
        Role::Client,
        None,
    ));
    let mut fragment_transport = AuthenticatedWebSocketTransport::from_authenticated_socket(
        fragment_client,
        Limits::default(),
    )
    .unwrap();
    block_on(fragment_server_io.write_all(&[0x02, 0x01, 1, 0x80, 0x01, 2])).unwrap();
    assert_eq!(
        block_on(fragment_transport.receive_binary(Limits::default().max_message_bytes)).unwrap(),
        vec![1, 2]
    );
}
