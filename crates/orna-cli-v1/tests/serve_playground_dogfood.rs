use std::{
    net::{TcpListener, TcpStream},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use orna_application_v1::LIVE_RUN_EVENTS_WATCH_SOURCE;
use orna_foundation_v1::{CanonicalValue, OvbRaw};
use orna_protocol_v1::{
    DatabaseContext, Envelope, Limits, Message, PresentKind, PresentNode, PresentPropertyKey,
    PresentationContext, ResultStatus, canonical_request_fingerprint,
};
use serde_json::{Value as JsonValue, json};
use tungstenite::{
    Message as WebSocketMessage,
    client::IntoClientRequest,
    http::{HeaderValue, header::HeaderName},
    stream::MaybeTlsStream,
};

const MAIN: &str = include_str!("fixtures/project-core-main.orna");
const SAMPLE: &str = include_str!("fixtures/playground-example.orna");
const SECOND_SAMPLE: &str = include_str!("fixtures/playground-concurrent-eval.orna");
const FOLLOWUP_SAMPLE: &str = include_str!("fixtures/playground-followup-eval.orna");
const PLAYGROUND_SCHEMA: &str = include_str!("fixtures/playground-schema.orna");
const ASSET_INDEX: &str = include_str!("fixtures/playground-asset-index.orna");
const ASSET_APP: &str = include_str!("fixtures/playground-asset-app.orna");
const ASSET_STYLE: &str = include_str!("fixtures/playground-asset-style.orna");
const BINARY: &str = env!("CARGO_BIN_EXE_orna-cli-v1");

struct RunningServer(Child);

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct HttpResponse {
    status: u16,
    headers: String,
    body: String,
}

fn run(command: &mut Command, description: &str) -> Output {
    let output = command.output().unwrap_or_else(|error| {
        panic!("{description} could not start: {error}");
    });
    assert!(
        output.status.success(),
        "{description} exited with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn git(project: &std::path::Path, arguments: &[&str]) {
    let mut command = Command::new("git");
    command.args(arguments).current_dir(project);
    let _ = run(&mut command, "Git fixture setup");
}

fn curl(url: &str, arguments: &[&str]) -> Result<HttpResponse, String> {
    let mut command = Command::new("curl");
    command
        .args([
            "--silent",
            "--show-error",
            "--include",
            "--noproxy",
            "*",
            "--max-time",
            "5",
        ])
        .args(arguments)
        .arg(url);
    let output = command.output().map_err(|error| format!("curl: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "curl exit {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let response = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let (headers, body) = response
        .split_once("\r\n\r\n")
        .or_else(|| response.split_once("\n\n"))
        .ok_or_else(|| "curl response had no header boundary".to_owned())?;
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| "curl response had no HTTP status".to_owned())?;
    Ok(HttpResponse {
        status,
        headers: headers.to_owned(),
        body: body.to_owned(),
    })
}

fn response_header<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        header_name
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
    })
}

fn uuid_bytes(uuid: &str) -> [u8; 16] {
    let compact = uuid.replace('-', "");
    assert_eq!(compact.len(), 32, "server returned a malformed UUID");
    let mut output = [0; 16];
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&compact[index * 2..index * 2 + 2], 16)
            .expect("server returned a hexadecimal UUID");
    }
    output
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("reserve a local test port")
        .local_addr()
        .expect("read test port")
        .port()
}

fn wait_until_serving(server: &mut RunningServer, base_url: &str) {
    let started = Instant::now();
    loop {
        if let Some(status) = server.0.try_wait().expect("check serving process") {
            panic!("orna serve exited before accepting HTTP requests: {status}");
        }
        if curl(&format!("{base_url}/"), &[]).is_ok() {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "orna serve did not start listening within 15 seconds"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

fn envelope(request: [u8; 16], watch: Option<[u8; 16]>, message: Message) -> Envelope {
    Envelope {
        request: Some(request),
        watch,
        message,
        extensions: Default::default(),
    }
}

fn evaluation(session: [u8; 16], database: [u8; 16], request: [u8; 16], source: &str) -> Envelope {
    let mut evaluation = envelope(
        request,
        None,
        Message::Eval {
            source: source.to_owned(),
            database: database_context(database),
            presentation: presentation(),
            fingerprint: [0; 32],
        },
    );
    let fingerprint = canonical_request_fingerprint(session, &evaluation, Limits::default())
        .expect("browser-compatible evaluation fingerprint");
    if let Message::Eval {
        fingerprint: sent, ..
    } = &mut evaluation.message
    {
        *sent = fingerprint;
    }
    evaluation
}

fn send_envelope(socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>, sent: Envelope) {
    socket
        .send(WebSocketMessage::Binary(
            sent.encode(Limits::default())
                .expect("encode browser protocol request")
                .into(),
        ))
        .expect("send browser protocol request");
}

fn read_envelope(socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>) -> Envelope {
    loop {
        let incoming = socket.read().expect("read browser protocol response");
        match incoming {
            WebSocketMessage::Binary(bytes) => {
                return Envelope::decode(&bytes, Limits::default())
                    .expect("server response is a valid Orna presentation envelope");
            }
            WebSocketMessage::Ping(bytes) => socket
                .send(WebSocketMessage::Pong(bytes))
                .expect("answer server ping"),
            WebSocketMessage::Close(frame) => {
                panic!("server closed the live session: {frame:?}");
            }
            WebSocketMessage::Text(_) | WebSocketMessage::Pong(_) | WebSocketMessage::Frame(_) => {
                continue;
            }
        }
    }
}

fn expected_run_events(events: &[(u64, u64)]) -> PresentNode {
    let children = events.iter().map(|(sequence, value)| {
        PresentNode::new(
            PresentKind::Name("run".into()),
            Some(orna_protocol_v1::PresentIdentity::Explicit(integer_value(
                *sequence,
            ))),
            [
                (PresentPropertyKey::Name("kind".into()), text_value("run")),
                (
                    PresentPropertyKey::Name("sequence".into()),
                    integer_value(*sequence),
                ),
                (
                    PresentPropertyKey::Name("status".into()),
                    text_value("success"),
                ),
                (
                    PresentPropertyKey::Name("value".into()),
                    integer_value(*value),
                ),
                (PresentPropertyKey::Name("stdout".into()), text_value("")),
            ],
            [],
        )
        .expect("construct expected run event presentation")
    });
    PresentNode::new(
        PresentKind::Name("run.events".into()),
        None,
        [(
            PresentPropertyKey::Name("count".into()),
            integer_value(
                u64::try_from(events.len()).expect("run event count fits its presentation"),
            ),
        )],
        children,
    )
    .expect("construct expected run events presentation")
}

fn integer_value(value: u64) -> CanonicalValue {
    CanonicalValue::new(OvbRaw::Int(value.into())).expect("integer is canonical")
}

fn text_value(value: &str) -> CanonicalValue {
    CanonicalValue::new(OvbRaw::Text(value.to_owned())).expect("text is canonical")
}

fn read_responses_for_requests(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    requests: &[[u8; 16]],
) -> Vec<Envelope> {
    let mut responses = Vec::with_capacity(requests.len());
    while responses.len() < requests.len() {
        let response = read_envelope(socket);
        if let Some(request) = response.request
            && requests.contains(&request)
        {
            assert!(
                responses
                    .iter()
                    .all(|received: &Envelope| received.request != Some(request)),
                "server returned a duplicate response for {request:?}"
            );
            responses.push(response);
        }
    }
    responses
}

fn read_delta_for_watch(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    watch: [u8; 16],
) -> Envelope {
    loop {
        let response = read_envelope(socket);
        if response.watch == Some(watch) && matches!(&response.message, Message::Delta { .. }) {
            return response;
        }
    }
}

fn presentation() -> PresentationContext {
    PresentationContext {
        locale: "en".into(),
        timezone: None,
        width: None,
        theme: "system".into(),
        supported_kinds: vec![
            "value".into(),
            "text".into(),
            "group".into(),
            "table".into(),
        ],
    }
}

fn database_context(database: [u8; 16]) -> DatabaseContext {
    DatabaseContext {
        database,
        snapshot: None,
    }
}

fn send_and_read_request(
    socket: &mut tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    sent: Envelope,
    expected_request: [u8; 16],
) -> Envelope {
    send_envelope(socket, sent);
    loop {
        let response = read_envelope(socket);
        if response.request == Some(expected_request) {
            return response;
        }
    }
}

#[test]
fn orna_serve_hosts_playground_with_pending_evals_and_snapshot_correct_deltas() {
    let project = tempfile::tempdir().expect("temporary Orna database");
    let initialized = Command::new(BINARY)
        .arg("init")
        .current_dir(project.path())
        .output()
        .expect("initialize the test Orna database");
    assert!(
        initialized.status.success(),
        "orna init exited with {}\n{}",
        initialized.status,
        String::from_utf8_lossy(&initialized.stderr),
    );

    std::fs::write(project.path().join("main.orna"), MAIN).expect("write crate-local main fixture");
    std::fs::write(project.path().join("playground.orna"), PLAYGROUND_SCHEMA)
        .expect("write crate-local playground schema");
    let example = project.path().join("playground/examples/hello.orna");
    std::fs::create_dir_all(example.parent().expect("example parent"))
        .expect("create committed example directory");
    std::fs::write(&example, SAMPLE).expect("write crate-local sample fixture");
    let assets = project.path().join("playground/Asset");
    std::fs::create_dir_all(&assets).expect("create committed asset directory");
    std::fs::write(assets.join("asset-696e6465782e68746d6c.orna"), ASSET_INDEX)
        .expect("write committed browser shell fixture");
    std::fs::write(
        assets.join("asset-6173736574732f6170702e6a73.orna"),
        ASSET_APP,
    )
    .expect("write committed browser client fixture");
    std::fs::write(
        assets.join("asset-6173736574732f7374796c652e637373.orna"),
        ASSET_STYLE,
    )
    .expect("write committed stylesheet fixture");
    git(
        project.path(),
        &[
            "add",
            "main.orna",
            "playground.orna",
            "playground/examples/hello.orna",
            "playground/Asset",
        ],
    );
    git(
        project.path(),
        &[
            "-c",
            "user.name=kierandrewett",
            "-c",
            "user.email=kieran@drewett.dev",
            "commit",
            "--quiet",
            "-m",
            "add dogfood fixtures",
        ],
    );

    let port = free_port();
    let child = Command::new(BINARY)
        .args(["serve", "--port", &port.to_string()])
        .current_dir(project.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start orna serve");
    let mut server = RunningServer(child);
    let base_url = format!("http://127.0.0.1:{port}");
    wait_until_serving(&mut server, &base_url);

    let listing = curl(&base_url, &[]).expect("curl the Git listing");
    assert_eq!(listing.status, 200);
    assert!(listing.body.contains("Orna database"));
    assert!(listing.body.contains("add dogfood fixtures"));
    assert!(listing.body.contains("href=\"/playground/\""));

    let page = curl(&format!("{base_url}/playground/"), &[]).expect("curl the playground page");
    assert_eq!(page.status, 200);
    assert!(page.body.contains("Orna playground"));
    assert!(page.body.contains("database shell"));
    assert!(page.body.contains("globalThis.ornaPlaygroundRun"));
    assert!(
        page.body
            .contains(LIVE_RUN_EVENTS_WATCH_SOURCE.trim_start_matches('\0'))
    );
    let app = curl(&format!("{base_url}/playground/assets/app.js"), &[])
        .expect("curl the playground browser asset");
    assert_eq!(app.status, 200);
    assert!(app.body.contains("ornaPlaygroundReady"));
    let stylesheet = curl(&format!("{base_url}/playground/assets/style.css"), &[])
        .expect("curl the committed database stylesheet");
    assert_eq!(stylesheet.status, 200);
    assert!(stylesheet.body.contains("#202122"));
    assert!(!project.path().join("playground/web-ui/dist").exists());

    let examples =
        curl(&format!("{base_url}/api/examples"), &[]).expect("curl committed playground examples");
    assert_eq!(examples.status, 200);
    let examples: JsonValue = serde_json::from_str(&examples.body).expect("example response JSON");
    assert_eq!(examples["examples"][0]["source"], SAMPLE);

    let database_id = listing
        .body
        .split_once("data-database=\"")
        .and_then(|(_, suffix)| suffix.split_once('"').map(|(value, _)| value))
        .expect("Git listing identifies its runtime database");
    let session_request = json!({"database": database_id, "protocol": "orna.present.v1"});
    let session_body = serde_json::to_string(&session_request).expect("encode session request");
    let session_response = curl(
        &format!("{base_url}/orna/session"),
        &[
            "--request",
            "POST",
            "--header",
            &format!("Origin: {base_url}"),
            "--header",
            "Content-Type: application/json",
            "--data",
            &session_body,
        ],
    )
    .expect("curl authenticated runtime session creation");
    assert_eq!(
        session_response.status, 201,
        "session creation response: {} headers {:?}",
        session_response.body, session_response.headers
    );
    let session: JsonValue =
        serde_json::from_str(&session_response.body).expect("session response JSON");
    let session_id = session["session"].as_str().expect("session UUID");
    let session_bytes = uuid_bytes(session_id);
    let websocket_path = session["websocket_path"]
        .as_str()
        .expect("session WebSocket path");
    let cookie = response_header(&session_response.headers, "set-cookie")
        .and_then(|value| value.split(';').next())
        .expect("session response cookie");

    let mut request = format!("ws://127.0.0.1:{port}{websocket_path}")
        .into_client_request()
        .expect("WebSocket request URL");
    request.headers_mut().insert(
        HeaderName::from_static("origin"),
        HeaderValue::from_bytes(base_url.as_bytes()).expect("WebSocket origin header"),
    );
    request.headers_mut().insert(
        HeaderName::from_static("cookie"),
        HeaderValue::from_bytes(cookie.as_bytes()).expect("session cookie header"),
    );
    request.headers_mut().insert(
        HeaderName::from_static("sec-websocket-protocol"),
        HeaderValue::from_static("orna.present.v1"),
    );
    request.headers_mut().insert(
        HeaderName::from_static("sec-websocket-key"),
        HeaderValue::from_static("+/v7+/v7+/v7+/v7+/v7+w=="),
    );
    let request_debug = format!("{request:?}");
    let (mut socket, upgrade) = tungstenite::connect(request).unwrap_or_else(|error| {
        panic!("open authenticated Orna presentation WebSocket: {error}; request {request_debug}");
    });
    assert_eq!(upgrade.status(), 101);
    if let MaybeTlsStream::Plain(stream) = socket.get_mut() {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("bound WebSocket response wait");
    }

    let watch_request = [0x31; 16];
    let watched = send_and_read_request(
        &mut socket,
        envelope(
            watch_request,
            None,
            Message::Watch {
                source: LIVE_RUN_EVENTS_WATCH_SOURCE.into(),
                database: database_context(uuid_bytes(database_id)),
                presentation: presentation(),
                refresh_floor: None,
            },
        ),
        watch_request,
    );
    let (watch_id, initial_present) = match watched.message {
        Message::Snapshot {
            revision: 0,
            present,
            ..
        } => (
            watched.watch.expect("live presentation watch identity"),
            present,
        ),
        response => panic!("run-events watch should start with a snapshot, got {response:?}"),
    };
    assert_eq!(initial_present, expected_run_events(&[]));

    let first_eval_request = [0x32; 16];
    let second_eval_request = [0x34; 16];
    // Keep both request IDs outstanding from the client perspective; neither
    // response is read until both fingerprinted Eval messages have been sent.
    send_envelope(
        &mut socket,
        evaluation(
            session_bytes,
            uuid_bytes(database_id),
            first_eval_request,
            SAMPLE.trim(),
        ),
    );
    send_envelope(
        &mut socket,
        evaluation(
            session_bytes,
            uuid_bytes(database_id),
            second_eval_request,
            SECOND_SAMPLE.trim(),
        ),
    );
    let results =
        read_responses_for_requests(&mut socket, &[first_eval_request, second_eval_request]);
    for result in results {
        let expected_value = if result.request == Some(first_eval_request) {
            2
        } else {
            assert_eq!(result.request, Some(second_eval_request));
            42
        };
        assert!(matches!(
            result.message,
            Message::Result {
                status: ResultStatus::Success,
                value: Some(value),
                ..
            } if value.raw() == &OvbRaw::Int(expected_value.into())
        ));
    }

    socket
        .send(WebSocketMessage::Binary(
            envelope([0x33; 16], Some(watch_id), Message::Resync)
                .encode(Limits::default())
                .expect("encode browser resync request")
                .into(),
        ))
        .expect("send browser resync request");
    // Resync responses are correlated to their watch and carry no request id.
    let delta = read_delta_for_watch(&mut socket, watch_id);
    let Message::Delta {
        base_revision,
        new_revision,
        patches,
        ..
    } = delta.message
    else {
        panic!("run event update should be a presentation delta");
    };
    assert_eq!((base_revision, new_revision), (0, 1));
    let first_updated_present = initial_present
        .apply_patches(&patches, Limits::default())
        .expect("live event delta applies to its initial snapshot");
    let first_completion_order = expected_run_events(&[(1, 2), (2, 42)]);
    let second_completion_order = expected_run_events(&[(1, 42), (2, 2)]);
    assert!(
        first_updated_present == first_completion_order
            || first_updated_present == second_completion_order,
        "the first delta should contain exactly both successful runs: {first_updated_present:?}"
    );

    let third_eval_request = [0x36; 16];
    let third_result = send_and_read_request(
        &mut socket,
        evaluation(
            session_bytes,
            uuid_bytes(database_id),
            third_eval_request,
            FOLLOWUP_SAMPLE.trim(),
        ),
        third_eval_request,
    );
    assert!(matches!(
        third_result.message,
        Message::Result {
            status: ResultStatus::Success,
            value: Some(value),
            ..
        } if value.raw() == &OvbRaw::Int(49.into())
    ));
    send_envelope(
        &mut socket,
        envelope([0x37; 16], Some(watch_id), Message::Resync),
    );
    let next_delta = read_delta_for_watch(&mut socket, watch_id);
    let Message::Delta {
        base_revision,
        new_revision,
        patches,
        ..
    } = next_delta.message
    else {
        panic!("the next run event update should be a presentation delta");
    };
    assert_eq!((base_revision, new_revision), (1, 2));
    let updated_present = first_updated_present
        .apply_patches(&patches, Limits::default())
        .expect("next live event delta applies to its matching snapshot");
    let first_completion_order = expected_run_events(&[(1, 2), (2, 42), (3, 49)]);
    let second_completion_order = expected_run_events(&[(1, 42), (2, 2), (3, 49)]);
    assert!(
        updated_present == first_completion_order || updated_present == second_completion_order,
        "successive deltas should preserve both runs and append the third: {updated_present:?}"
    );

    let fresh_watch_request = [0x35; 16];
    let fresh_watch = send_and_read_request(
        &mut socket,
        envelope(
            fresh_watch_request,
            None,
            Message::Watch {
                source: LIVE_RUN_EVENTS_WATCH_SOURCE.into(),
                database: database_context(uuid_bytes(database_id)),
                presentation: presentation(),
                refresh_floor: None,
            },
        ),
        fresh_watch_request,
    );
    assert!(matches!(
        fresh_watch.message,
        Message::Snapshot {
            revision: 0,
            present,
            ..
        } if present == updated_present
    ));

    println!(
        "curl GET / -> HTTP {} (exit 0): Git listing + playground link",
        listing.status
    );
    println!(
        "curl GET /playground/ -> HTTP {} (exit 0): served browser page + runtime bridge",
        page.status
    );
    println!(
        "curl GET /playground/assets/app.js -> HTTP {} (exit 0): browser asset",
        app.status
    );
    println!(
        "curl GET /api/examples -> HTTP {} (exit 0): committed .orna sample",
        200
    );
    println!(
        "curl POST /orna/session -> HTTP {} (exit 0): session + cookie",
        session_response.status
    );
    println!("WebSocket WATCH -> Snapshot revision 0 (exit 0)");
    println!("WebSocket EVAL x2 outstanding -> correlated Results 2 and 42 (exit 0)");
    println!("WebSocket RESYNC -> Delta revision 0..1 (exit 0): both pipelined run events");
    println!("WebSocket RESYNC -> Delta revision 1..2 (exit 0): next run event applies");
    println!("WebSocket WATCH -> fresh Snapshot revision 0 (exit 0): matches applied delta");
}
