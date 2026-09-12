//! Client-owned authenticated live-session transport.
//!
//! This module owns session HTTP credentials and the client side of the
//! binary WebSocket message boundary.  Tokio runtime lifetime remains owned
//! by the embedding application.

use std::{collections::HashSet, fmt, time::Duration};

use futures_util::{SinkExt, StreamExt};
use reqwest::{Client as HttpClient, StatusCode, Url};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use orna_protocol_v1::Limits;

use crate::live_session::{AuthenticatedLiveTransport, LiveByteDriver};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlsPolicy {
    RequireTls,
    TrustedLoopbackOnly,
}

#[derive(Clone, Debug)]
pub struct LiveClientConfig {
    pub endpoint: Url,
    pub origin: String,
    pub limits: Limits,
    pub request_timeout: Duration,
    pub tls_policy: TlsPolicy,
}

impl LiveClientConfig {
    pub fn validate(&self) -> Result<(), LiveTransportError> {
        let scheme = self.endpoint.scheme();
        if scheme != "https" && scheme != "http" {
            return Err(LiveTransportError::Configuration(
                "endpoint must use http or https",
            ));
        }
        if self.origin.is_empty() || self.request_timeout.is_zero() {
            return Err(LiveTransportError::Configuration(
                "origin and timeout are required",
            ));
        }
        self.limits
            .validate()
            .map_err(|_| LiveTransportError::Configuration("invalid protocol limits"))?;
        if scheme == "http"
            && (self.tls_policy == TlsPolicy::RequireTls || !is_loopback(&self.endpoint))
        {
            return Err(LiveTransportError::Configuration(
                "plaintext live transport is restricted to trusted loopback",
            ));
        }
        Ok(())
    }
}

fn is_loopback(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    )
}

#[derive(Debug)]
pub enum LiveTransportError {
    Configuration(&'static str),
    Http(reqwest::Error),
    HttpStatus(StatusCode),
    Response(&'static str),
    WebSocket(tokio_tungstenite::tungstenite::Error),
    Protocol(orna_protocol_v1::Error),
}

impl fmt::Display for LiveTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(message) | Self::Response(message) => f.write_str(message),
            Self::Http(error) => error.fmt(f),
            Self::HttpStatus(status) => write!(f, "live session HTTP status {status}"),
            Self::WebSocket(error) => error.fmt(f),
            Self::Protocol(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for LiveTransportError {}

/// Secret-bearing session state. It deliberately has no public fields or
/// Debug implementation; callers can inspect only non-secret session facts.
pub struct LiveSession {
    session_id: [u8; 16],
    database_id: [u8; 16],
    runtime_id: [u8; 16],
    websocket_path: String,
    resume_token: String,
    cookie: String,
    limits: Limits,
}

impl LiveSession {
    pub const fn session_id(&self) -> [u8; 16] {
        self.session_id
    }
    pub const fn database_id(&self) -> [u8; 16] {
        self.database_id
    }
    pub const fn runtime_id(&self) -> [u8; 16] {
        self.runtime_id
    }
    pub const fn limits(&self) -> Limits {
        self.limits
    }
}

pub struct LiveClient {
    config: LiveClientConfig,
    http: HttpClient,
}

impl LiveClient {
    pub fn new(config: LiveClientConfig) -> Result<Self, LiveTransportError> {
        config.validate()?;
        let http = HttpClient::builder()
            .timeout(config.request_timeout)
            .build()
            .map_err(LiveTransportError::Http)?;
        Ok(Self { config, http })
    }

    pub async fn create_session(
        &self,
        database: [u8; 16],
    ) -> Result<LiveSession, LiveTransportError> {
        let endpoint = self
            .config
            .endpoint
            .join("/orna/session")
            .map_err(|_| LiveTransportError::Configuration("invalid session endpoint"))?;
        let body = serde_json::json!({
            "database": hex_uuid(database),
            "protocol": "orna.present.v1",
        });
        self.session_request(endpoint, body, None).await
    }

    pub async fn resume_session(
        &self,
        session: &LiveSession,
    ) -> Result<LiveSession, LiveTransportError> {
        let endpoint = self
            .config
            .endpoint
            .join(&format!(
                "/orna/session/{}/resume",
                hex_uuid(session.session_id)
            ))
            .map_err(|_| LiveTransportError::Configuration("invalid resume endpoint"))?;
        let body = serde_json::json!({ "resume_token": session.resume_token, "protocol": "orna.present.v1" });
        let replacement = self.session_request(endpoint, body, Some(session)).await?;
        if replacement.resume_token == session.resume_token || replacement.cookie == session.cookie
        {
            return Err(LiveTransportError::Response(
                "resume response did not rotate credentials",
            ));
        }
        Ok(replacement)
    }

    /// Deletes a session using its current bearer credential. Cookies are
    /// intentionally not sent on this destructive HTTP operation.
    pub async fn delete_session(&self, session: &LiveSession) -> Result<(), LiveTransportError> {
        let endpoint = self
            .config
            .endpoint
            .join(&format!("/orna/session/{}", hex_uuid(session.session_id)))
            .map_err(|_| LiveTransportError::Configuration("invalid delete endpoint"))?;
        let response = self
            .http
            .delete(endpoint)
            .header("origin", &self.config.origin)
            .bearer_auth(&session.resume_token)
            .send()
            .await
            .map_err(LiveTransportError::Http)?;
        if response.status() != StatusCode::NO_CONTENT {
            return Err(LiveTransportError::HttpStatus(response.status()));
        }
        Ok(())
    }

    async fn session_request(
        &self,
        endpoint: Url,
        body: serde_json::Value,
        old: Option<&LiveSession>,
    ) -> Result<LiveSession, LiveTransportError> {
        let response = self
            .http
            .post(endpoint)
            .header("origin", &self.config.origin)
            .json(&body)
            .send()
            .await
            .map_err(LiveTransportError::Http)?;
        let expected_status = if old.is_some() {
            StatusCode::OK
        } else {
            StatusCode::CREATED
        };
        if response.status() != expected_status {
            return Err(LiveTransportError::HttpStatus(response.status()));
        }
        let set_cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
            .ok_or(LiveTransportError::Response(
                "session response omitted cookie",
            ))?;
        let body = response.bytes().await.map_err(LiveTransportError::Http)?;
        reject_duplicate_json_members(&body)?;
        let value: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|_| LiveTransportError::Response("invalid session JSON"))?;
        let session = parse_session(
            value,
            &set_cookie,
            self.config.limits,
            self.config.endpoint.scheme() == "https",
        )?;
        if let Some(old) = old
            && (session.session_id != old.session_id || session.database_id != old.database_id)
        {
            return Err(LiveTransportError::Response(
                "resume response changed session identity",
            ));
        }
        Ok(session)
    }

    pub async fn connect(
        &self,
        session: &LiveSession,
    ) -> Result<AuthenticatedWebSocketTransport<MaybeTlsStream<TcpStream>>, LiveTransportError>
    {
        let ws_scheme = if self.config.endpoint.scheme() == "https" {
            "wss"
        } else {
            "ws"
        };
        let mut endpoint = self.config.endpoint.clone();
        endpoint
            .set_scheme(ws_scheme)
            .map_err(|_| LiveTransportError::Configuration("invalid WebSocket scheme"))?;
        let url = bind_socket_path(&endpoint, &session.websocket_path)?;
        let mut request = url
            .as_str()
            .into_client_request()
            .map_err(|_| LiveTransportError::Configuration("invalid WebSocket request"))?;
        request.headers_mut().insert(
            "origin",
            self.config
                .origin
                .parse()
                .map_err(|_| LiveTransportError::Configuration("invalid origin"))?,
        );
        request.headers_mut().insert(
            "cookie",
            session
                .cookie
                .parse()
                .map_err(|_| LiveTransportError::Configuration("invalid session cookie"))?,
        );
        request
            .headers_mut()
            .insert("sec-websocket-protocol", "orna.present.v1".parse().unwrap());
        let (socket, response) = connect_async(request)
            .await
            .map_err(LiveTransportError::WebSocket)?;
        if response
            .headers()
            .get("sec-websocket-protocol")
            .and_then(|value| value.to_str().ok())
            != Some("orna.present.v1")
        {
            return Err(LiveTransportError::Response(
                "server did not negotiate orna.present.v1",
            ));
        }
        Ok(AuthenticatedWebSocketTransport {
            socket,
            max_message_bytes: session.limits.max_message_bytes,
        })
    }
}

fn parse_session(
    value: serde_json::Value,
    set_cookie: &str,
    limits: Limits,
    secure_transport: bool,
) -> Result<LiveSession, LiveTransportError> {
    let object = value.as_object().ok_or(LiveTransportError::Response(
        "session response is not an object",
    ))?;
    const SESSION_FIELDS: &[&str] = &[
        "session",
        "database",
        "runtime",
        "resume_token",
        "websocket_path",
        "lease_ms",
        "limits",
    ];
    if object.len() != SESSION_FIELDS.len()
        || object
            .keys()
            .any(|key| !SESSION_FIELDS.contains(&key.as_str()))
    {
        return Err(LiveTransportError::Response(
            "session response has unknown fields",
        ));
    }
    let string = |name: &'static str| {
        object
            .get(name)
            .and_then(serde_json::Value::as_str)
            .ok_or(LiveTransportError::Response(name))
    };
    let session_id = parse_id(string("session")?)?;
    let database_id = parse_id(string("database")?)?;
    let runtime_id = parse_id(string("runtime")?)?;
    let websocket_path = string("websocket_path")?.to_owned();
    let resume_token = string("resume_token")?.to_owned();
    validate_resume_token(&resume_token)?;
    validate_socket_path_for_session(&websocket_path, session_id)?;
    let lease_ms = positive_bounded_u64(object.get("lease_ms"), "lease_ms", 300_000)?;
    let response_limits = parse_response_limits(object.get("limits"), limits, lease_ms)?;
    let cookie = parse_set_cookie(set_cookie, &websocket_path, secure_transport)?;
    let cookie_value = cookie
        .split_once('=')
        .map(|(_, value)| value)
        .ok_or(LiveTransportError::Response("invalid session cookie"))?;
    if cookie_value == resume_token {
        return Err(LiveTransportError::Response(
            "session cookie must differ from resume token",
        ));
    }
    Ok(LiveSession {
        session_id,
        database_id,
        runtime_id,
        websocket_path,
        resume_token,
        cookie,
        limits: response_limits,
    })
}

fn validate_resume_token(token: &str) -> Result<(), LiveTransportError> {
    if token.len() != 43
        || !token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(LiveTransportError::Response("invalid resume token"));
    }
    Ok(())
}

fn validate_socket_path_for_session(
    path: &str,
    session: [u8; 16],
) -> Result<(), LiveTransportError> {
    let expected = format!("/orna/live/{}", hex_uuid(session));
    if path != expected {
        return Err(LiveTransportError::Response(
            "WebSocket path does not match session",
        ));
    }
    Ok(())
}

fn positive_bounded_u64(
    value: Option<&serde_json::Value>,
    name: &'static str,
    maximum: u64,
) -> Result<u64, LiveTransportError> {
    let value = value
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value > 0 && *value <= maximum)
        .ok_or(LiveTransportError::Response(name))?;
    Ok(value)
}

fn parse_response_limits(
    value: Option<&serde_json::Value>,
    configured: Limits,
    lease_ms: u64,
) -> Result<Limits, LiveTransportError> {
    let object = value
        .and_then(serde_json::Value::as_object)
        .ok_or(LiveTransportError::Response("limits"))?;
    const LIMIT_FIELDS: &[&str] = &[
        "max_message_bytes",
        "max_depth",
        "max_nodes",
        "max_collection_items",
        "max_outgoing_bytes",
        "request_retention_ms",
    ];
    if object.len() != LIMIT_FIELDS.len()
        || object
            .keys()
            .any(|key| !LIMIT_FIELDS.contains(&key.as_str()))
    {
        return Err(LiveTransportError::Response("limits has unknown fields"));
    }
    let max_message_bytes = positive_bounded_usize(
        object.get("max_message_bytes"),
        "max_message_bytes",
        configured.max_message_bytes,
    )?;
    let max_depth =
        positive_bounded_usize(object.get("max_depth"), "max_depth", configured.max_depth)?;
    let max_nodes =
        positive_bounded_usize(object.get("max_nodes"), "max_nodes", configured.max_nodes)?;
    let max_collection_items = positive_bounded_usize(
        object.get("max_collection_items"),
        "max_collection_items",
        configured.max_collection_items,
    )?;
    let _max_outgoing_bytes = positive_bounded_usize(
        object.get("max_outgoing_bytes"),
        "max_outgoing_bytes",
        configured.max_message_bytes,
    )?;
    let request_retention_ms = positive_bounded_u64(
        object.get("request_retention_ms"),
        "request_retention_ms",
        u64::MAX,
    )?;
    if request_retention_ms < lease_ms {
        return Err(LiveTransportError::Response(
            "request retention is shorter than lease",
        ));
    }
    Ok(Limits {
        max_message_bytes,
        max_depth,
        max_nodes,
        max_collection_items,
    })
}

fn positive_bounded_usize(
    value: Option<&serde_json::Value>,
    name: &'static str,
    maximum: usize,
) -> Result<usize, LiveTransportError> {
    let value = value
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0 && *value <= maximum)
        .ok_or(LiveTransportError::Response(name))?;
    Ok(value)
}

fn parse_set_cookie(
    value: &str,
    websocket_path: &str,
    secure_transport: bool,
) -> Result<String, LiveTransportError> {
    let mut parts = value.split(';').map(str::trim);
    let pair = parts
        .next()
        .ok_or(LiveTransportError::Response("invalid session cookie"))?;
    let (name, cookie_value) = pair
        .split_once('=')
        .ok_or(LiveTransportError::Response("invalid session cookie"))?;
    if name != "orna_session"
        || cookie_value.is_empty()
        || cookie_value.bytes().any(|byte| {
            byte.is_ascii_whitespace() || byte.is_ascii_control() || matches!(byte, b';' | b',')
        })
    {
        return Err(LiveTransportError::Response("invalid session cookie"));
    }
    let mut path_ok = false;
    let mut http_only = false;
    let mut same_site_strict = false;
    let mut secure = false;
    for attribute in parts {
        if attribute.eq_ignore_ascii_case("httponly") {
            if http_only {
                return Err(LiveTransportError::Response("invalid session cookie"));
            }
            http_only = true;
        } else if attribute.eq_ignore_ascii_case("secure") {
            if secure {
                return Err(LiveTransportError::Response("invalid session cookie"));
            }
            secure = true;
        } else if let Some(path) = attribute.strip_prefix("Path=") {
            if path_ok || path != websocket_path {
                return Err(LiveTransportError::Response("invalid session cookie"));
            }
            path_ok = true;
        } else if attribute.eq_ignore_ascii_case("SameSite=Strict") {
            if same_site_strict {
                return Err(LiveTransportError::Response("invalid session cookie"));
            }
            same_site_strict = true;
        } else {
            return Err(LiveTransportError::Response("invalid session cookie"));
        }
    }
    if !path_ok || !http_only || !same_site_strict || secure != secure_transport {
        return Err(LiveTransportError::Response("invalid session cookie"));
    }
    Ok(pair.to_owned())
}

fn json_whitespace(byte: u8) -> bool {
    matches!(byte, b' ' | b'\n' | b'\r' | b'\t')
}

fn reject_duplicate_json_members(input: &[u8]) -> Result<(), LiveTransportError> {
    fn string_end(input: &[u8], mut index: usize) -> Option<usize> {
        if input.get(index)? != &b'"' {
            return None;
        }
        index += 1;
        while index < input.len() {
            match input[index] {
                b'"' => return Some(index + 1),
                b'\\' => index += 2,
                byte if byte < 0x20 => return None,
                _ => index += 1,
            }
        }
        None
    }
    fn value(input: &[u8], mut index: usize) -> Option<usize> {
        while input.get(index).is_some_and(|byte| json_whitespace(*byte)) {
            index += 1;
        }
        match input.get(index)? {
            b'{' => object(input, index + 1),
            b'[' => {
                index += 1;
                while input.get(index).is_some_and(|byte| json_whitespace(*byte)) {
                    index += 1;
                }
                if input.get(index) == Some(&b']') {
                    return Some(index + 1);
                }
                loop {
                    index = value(input, index)?;
                    while input.get(index).is_some_and(|byte| json_whitespace(*byte)) {
                        index += 1;
                    }
                    match input.get(index)? {
                        b',' => index += 1,
                        b']' => return Some(index + 1),
                        _ => return None,
                    }
                }
            }
            b'"' => string_end(input, index),
            _ => {
                while input.get(index).is_some_and(|byte| {
                    !json_whitespace(*byte) && !matches!(byte, b',' | b']' | b'}')
                }) {
                    index += 1;
                }
                (index > 0).then_some(index)
            }
        }
    }
    fn object(input: &[u8], mut index: usize) -> Option<usize> {
        let mut keys = HashSet::new();
        while input.get(index).is_some_and(|byte| json_whitespace(*byte)) {
            index += 1;
        }
        if input.get(index) == Some(&b'}') {
            return Some(index + 1);
        }
        loop {
            while input.get(index).is_some_and(|byte| json_whitespace(*byte)) {
                index += 1;
            }
            let end = string_end(input, index)?;
            let key: String = serde_json::from_slice(&input[index..end]).ok()?;
            if !keys.insert(key) {
                return None;
            }
            index = end;
            while input.get(index).is_some_and(|byte| json_whitespace(*byte)) {
                index += 1;
            }
            if input.get(index)? != &b':' {
                return None;
            }
            index = value(input, index + 1)?;
            while input.get(index).is_some_and(|byte| json_whitespace(*byte)) {
                index += 1;
            }
            match input.get(index)? {
                b',' => index += 1,
                b'}' => return Some(index + 1),
                _ => return None,
            }
        }
    }
    let end = value(input, 0).ok_or(LiveTransportError::Response(
        "duplicate or malformed session JSON",
    ))?;
    let mut index = end;
    while input.get(index).is_some_and(|byte| json_whitespace(*byte)) {
        index += 1;
    }
    if index != input.len() {
        return Err(LiveTransportError::Response("invalid session JSON"));
    }
    Ok(())
}

fn bind_socket_path(endpoint: &Url, advertised: &str) -> Result<Url, LiveTransportError> {
    if !advertised.starts_with('/')
        || advertised.starts_with("//")
        || advertised
            .bytes()
            .any(|byte| matches!(byte, b'\\' | b'?' | b'#' | b'%'))
        || advertised
            .split('/')
            .any(|segment| segment == "." || segment == "..")
    {
        return Err(LiveTransportError::Response(
            "invalid absolute WebSocket path",
        ));
    }
    let url = endpoint
        .join(advertised)
        .map_err(|_| LiveTransportError::Response("invalid absolute WebSocket path"))?;
    if url.origin() != endpoint.origin() || !url.username().is_empty() || url.password().is_some() {
        return Err(LiveTransportError::Response(
            "WebSocket path escaped configured origin",
        ));
    }
    Ok(url)
}

fn parse_id(value: &str) -> Result<[u8; 16], LiveTransportError> {
    let bytes = value.as_bytes();
    if bytes.len() != 36 || ![8, 13, 18, 23].iter().all(|&index| bytes[index] == b'-') {
        return Err(LiveTransportError::Response("invalid session identifier"));
    }
    let mut output = [0; 16];
    let mut output_index = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'-' {
            index += 1;
            continue;
        }
        output[output_index] = (hex(bytes[index])? << 4) | hex(bytes[index + 1])?;
        output_index += 1;
        index += 2;
    }
    Ok(output)
}

fn hex(value: u8) -> Result<u8, LiveTransportError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(LiveTransportError::Response("invalid session identifier")),
    }
}

fn hex_uuid(value: [u8; 16]) -> String {
    let mut output = String::with_capacity(36);
    for (index, byte) in value.into_iter().enumerate() {
        if matches!(index, 4 | 6 | 8 | 10) {
            output.push('-');
        }
        output.push(char::from(b"0123456789abcdef"[(byte >> 4) as usize]));
        output.push(char::from(b"0123456789abcdef"[(byte & 15) as usize]));
    }
    output
}

pub struct AuthenticatedWebSocketTransport<S> {
    socket: WebSocketStream<S>,
    max_message_bytes: usize,
}

impl<S> AuthenticatedWebSocketTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Wraps a stream whose authentication and `orna.present.v1` handshake
    /// have already completed.
    pub fn from_authenticated_socket(
        socket: WebSocketStream<S>,
        limits: Limits,
    ) -> Result<Self, LiveTransportError> {
        limits
            .validate()
            .map_err(|_| LiveTransportError::Configuration("invalid protocol limits"))?;
        Ok(Self {
            socket,
            max_message_bytes: limits.max_message_bytes,
        })
    }
}

impl<S> LiveByteDriver for AuthenticatedWebSocketTransport<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    type Error = LiveTransportError;

    fn receive_binary<'a>(
        &'a mut self,
        max_bytes: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>, Self::Error>> + 'a>>
    {
        Box::pin(async move {
            let bound = self.max_message_bytes.min(max_bytes);
            loop {
                match self.socket.next().await {
                    Some(Ok(Message::Binary(bytes))) => {
                        let bytes = bytes.to_vec();
                        if bytes.len() > bound {
                            return Err(LiveTransportError::Protocol(
                                orna_protocol_v1::Error::Limit,
                            ));
                        }
                        return Ok(bytes);
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        self.socket
                            .send(Message::Pong(payload))
                            .await
                            .map_err(LiveTransportError::WebSocket)?;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None => {
                        return Err(LiveTransportError::WebSocket(
                            tokio_tungstenite::tungstenite::Error::ConnectionClosed,
                        ));
                    }
                    Some(Ok(Message::Text(_))) => {
                        return Err(LiveTransportError::Response(
                            "text WebSocket message rejected",
                        ));
                    }
                    Some(Ok(Message::Frame(_))) => {}
                    Some(Err(error)) => return Err(LiveTransportError::WebSocket(error)),
                }
            }
        })
    }

    fn send_binary<'a>(
        &'a mut self,
        bytes: Vec<u8>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), Self::Error>> + 'a>> {
        Box::pin(async move {
            if bytes.len() > self.max_message_bytes {
                return Err(LiveTransportError::Protocol(orna_protocol_v1::Error::Limit));
            }
            self.socket
                .send(Message::Binary(bytes.into()))
                .await
                .map_err(LiveTransportError::WebSocket)
        })
    }
}

impl<S> AuthenticatedLiveTransport for AuthenticatedWebSocketTransport<S> where
    S: AsyncRead + AsyncWrite + Unpin
{
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_token() -> String {
        "A".repeat(43)
    }

    fn valid_response() -> serde_json::Value {
        serde_json::json!({
            "session": "00000000-0000-0000-0000-000000000001",
            "database": "00000000-0000-0000-0000-000000000002",
            "runtime": "00000000-0000-0000-0000-000000000003",
            "resume_token": valid_token(),
            "websocket_path": "/orna/live/00000000-0000-0000-0000-000000000001",
            "lease_ms": 30_000,
            "limits": {
                "max_message_bytes": 16 * 1024 * 1024,
                "max_depth": 64,
                "max_nodes": 100_000,
                "max_collection_items": 100_000,
                "max_outgoing_bytes": 16 * 1024 * 1024,
                "request_retention_ms": 30_000,
            },
        })
    }

    fn valid_cookie() -> String {
        "orna_session=opaque-cookie; Path=/orna/live/00000000-0000-0000-0000-000000000001; HttpOnly; SameSite=Strict; Secure".into()
    }

    #[test]
    fn advertised_socket_locator_cannot_route_credentials_elsewhere() {
        let endpoint = Url::parse("https://trusted.example/base").unwrap();
        for path in [
            "https://evil.example/steal",
            "//evil.example/steal",
            "/../steal",
            "/safe%2f..%2fsteal",
            "/safe?next=https://evil.example",
            "/safe#fragment",
        ] {
            assert!(bind_socket_path(&endpoint, path).is_err(), "{path}");
        }
        let safe = bind_socket_path(&endpoint, "/orna/live/session").unwrap();
        assert_eq!(safe.origin(), endpoint.origin());
        assert_eq!(safe.host_str(), endpoint.host_str());
    }

    #[test]
    fn session_parser_rejects_malformed_ids_tokens_and_cookies() {
        let token = valid_token();
        let cookie = valid_cookie();
        let parsed = parse_session(valid_response(), &cookie, Limits::default(), true).unwrap();
        assert_eq!(
            parsed.session_id,
            [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(parsed.cookie, "orna_session=opaque-cookie");

        let mut malformed = valid_response();
        malformed["session"] = serde_json::Value::String("not-an-id".into());
        assert!(parse_session(malformed, &cookie, Limits::default(), true).is_err());
        let opaque_cookie = "orna_session=wrong; Path=/orna/live/00000000-0000-0000-0000-000000000001; HttpOnly; SameSite=Strict; Secure";
        assert_eq!(
            parse_session(valid_response(), opaque_cookie, Limits::default(), true)
                .unwrap()
                .cookie,
            "orna_session=wrong"
        );
        assert!(
            parse_session(
                valid_response(),
                &format!("orna_session={token}; Path=/orna/live/00000000-0000-0000-0000-000000000001; HttpOnly; SameSite=Strict; Secure; Domain=evil.example"),
                Limits::default(),
                true,
            )
            .is_err()
        );
        assert!(
            parse_session(
                valid_response(),
                "orna_session=opaque-cookie; Path=/orna/live/00000000-0000-0000-0000-000000000001; HttpOnly; Secure",
                Limits::default(),
                true,
            )
            .is_err()
        );
    }

    #[test]
    fn session_response_requires_exact_schema_and_rotatable_opaque_credentials() {
        assert!(reject_duplicate_json_members(br#"{"session":1,"session":2}"#).is_err());
        let mut unknown = valid_response();
        unknown["unexpected"] = serde_json::Value::Bool(true);
        assert!(parse_session(unknown, &valid_cookie(), Limits::default(), true).is_err());

        let mut wrong_path = valid_response();
        wrong_path["websocket_path"] = "/orna/live/00000000-0000-0000-0000-000000000009".into();
        assert!(parse_session(wrong_path, &valid_cookie(), Limits::default(), true).is_err());

        let mut short_retention = valid_response();
        short_retention["limits"]["request_retention_ms"] = 29_999.into();
        assert!(parse_session(short_retention, &valid_cookie(), Limits::default(), true).is_err());

        let mut invalid_lease = valid_response();
        invalid_lease["lease_ms"] = 0.into();
        assert!(parse_session(invalid_lease, &valid_cookie(), Limits::default(), true).is_err());

        let distinct =
            parse_session(valid_response(), &valid_cookie(), Limits::default(), true).unwrap();
        assert_ne!(distinct.cookie, format!("orna_session={}", valid_token()));
    }
}
