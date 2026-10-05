use std::{
    collections::BTreeMap,
    net::{TcpListener, TcpStream},
    process::{Child, Command, Output, Stdio},
    sync::{Arc, Barrier},
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
const SAMPLE_ROW: &str = include_str!("fixtures/playground-sample.orna");
const LIVE_PROGRAM_ROW: &str = include_str!("fixtures/playground-sample-live.orna");
const SAMPLE_ARITHMETIC: &str = include_str!("fixtures/playground-sample-arithmetic.orna");
const SAMPLE_FUNCTIONS: &str = include_str!("fixtures/playground-sample-functions.orna");
const SAMPLE_INCREMENT: &str = include_str!("fixtures/playground-sample-increment.orna");
const SECOND_SAMPLE: &str = include_str!("fixtures/playground-concurrent-eval.orna");
const FOLLOWUP_SAMPLE: &str = include_str!("fixtures/playground-followup-eval.orna");
const PLAYGROUND_SCHEMA: &str = include_str!("fixtures/playground-schema.orna");
const ASSET_INDEX: &str = include_str!("fixtures/playground-asset-index.orna");
const ASSET_APP: &str = include_str!("fixtures/playground-asset-app.orna");
const ASSET_STYLE: &str = include_str!("fixtures/playground-asset-style.orna");
const PLAYGROUND_THEME: &str = include_str!("fixtures/playground-theme.orna");
const PLAYGROUND_THEME_UPDATED: &str = include_str!("fixtures/playground-theme-updated.orna");
const PLAYGROUND_LAYOUT: &str = include_str!("fixtures/playground-layout.orna");
const PLAYGROUND_LAYOUT_UPDATED: &str = include_str!("fixtures/playground-layout-updated.orna");
const ASSET_PRESENTATION: &str = include_str!("fixtures/playground-asset-presentation.orna");
const ASSET_HOME: &str = include_str!("fixtures/playground-asset-home.orna");
const ASSET_PLAYGROUND: &str = include_str!("fixtures/playground-asset-playground.orna");
const ASSET_LSP_JS: &str = include_str!("fixtures/playground-asset-lsp-js.orna");
const ASSET_LSP_WASM: &str = include_str!("fixtures/playground-asset-lsp-wasm.orna");
const ASSET_EDITOR_CONFIG: &str = include_str!("fixtures/playground-asset-editor-config.orna");
const ASSET_EMBED: &str = include_str!("fixtures/playground-asset-embed.orna");
const ASSET_EXAMPLE_CATALOG: &str = include_str!("fixtures/playground-asset-example-catalog.orna");
const ASSET_EXAMPLE_CATALOG_SCRIPT: &str =
    include_str!("fixtures/playground-asset-example-catalog-script.orna");
const ASSET_EXAMPLE_CATALOG_STYLE: &str =
    include_str!("fixtures/playground-asset-example-catalog-style.orna");
const ROUTE_PAGE: &str = include_str!("fixtures/playground-route-page.orna");
const ROUTE_EMBED: &str = include_str!("fixtures/playground-route-embed.orna");
const ROUTE_APP: &str = include_str!("fixtures/playground-route-app.orna");
const ROUTE_STYLE: &str = include_str!("fixtures/playground-route-style.orna");
const ROUTE_CONFIG: &str = include_str!("fixtures/playground-route-config.orna");
const ROUTE_EMBED_SCRIPT: &str = include_str!("fixtures/playground-route-embed-script.orna");
const ROUTE_LIVE: &str = include_str!("fixtures/playground-route-live.orna");
const ROUTE_LIVE_ASSET: &str = include_str!("fixtures/playground-route-live-asset.orna");
const ENTRY_PAGE: &str = include_str!("fixtures/playground-entry-page.orna");
const ENTRY_EMBED: &str = include_str!("fixtures/playground-entry-embed.orna");
const ENTRY_APP: &str = include_str!("fixtures/playground-entry-app.orna");
const ENTRY_STYLE: &str = include_str!("fixtures/playground-entry-style.orna");
const ENTRY_CONFIG: &str = include_str!("fixtures/playground-entry-config.orna");
const ENTRY_EMBED_SCRIPT: &str = include_str!("fixtures/playground-entry-embed-script.orna");
const ENTRY_LIVE: &str = include_str!("fixtures/playground-entry-live.orna");
const ENTRY_LIVE_ASSET: &str = include_str!("fixtures/playground-entry-live-asset.orna");
const ROUTE_PRESENTATION: &str = include_str!("fixtures/playground-route-presentation.orna");
const ROUTE_HOME_RUNTIME: &str = include_str!("fixtures/playground-route-home-runtime.orna");
const ROUTE_PLAYGROUND_RUNTIME: &str =
    include_str!("fixtures/playground-route-playground-runtime.orna");
const ROUTE_LSP_JS: &str = include_str!("fixtures/playground-route-lsp-js.orna");
const ROUTE_LSP_WASM: &str = include_str!("fixtures/playground-route-lsp-wasm.orna");
const ROUTE_EXAMPLE_CATALOG: &str = include_str!("fixtures/playground-route-example-catalog.orna");
const ROUTE_EXAMPLE_CATALOG_ASSET: &str =
    include_str!("fixtures/playground-route-example-catalog-asset.orna");
const ROUTE_EXAMPLE_CATALOG_SCRIPT: &str =
    include_str!("fixtures/playground-route-example-catalog-script.orna");
const ROUTE_EXAMPLE_CATALOG_STYLE: &str =
    include_str!("fixtures/playground-route-example-catalog-style.orna");
const LIVE_SNAPSHOT_ROUTE_TEMPLATE: &str =
    include_str!("fixtures/playground-route-live-snapshot.orna");
const LIVE_SNAPSHOT_ENTRY_TEMPLATE: &str =
    include_str!("fixtures/playground-entry-live-snapshot.orna");
const LIVE_SNAPSHOT_ASSET_TEMPLATE: &str =
    include_str!("fixtures/playground-asset-live-snapshot.orna");
const LIVE_SNAPSHOT_SAMPLE_TEMPLATE: &str =
    include_str!("fixtures/playground-sample-live-snapshot.orna");
const ENTRY_PRESENTATION: &str = include_str!("fixtures/playground-entry-presentation.orna");
const ENTRY_HOME_RUNTIME: &str = include_str!("fixtures/playground-entry-home-runtime.orna");
const ENTRY_PLAYGROUND_RUNTIME: &str =
    include_str!("fixtures/playground-entry-playground-runtime.orna");
const ENTRY_LSP_JS: &str = include_str!("fixtures/playground-entry-lsp-js.orna");
const ENTRY_LSP_WASM: &str = include_str!("fixtures/playground-entry-lsp-wasm.orna");
const ENTRY_EXAMPLE_CATALOG: &str = include_str!("fixtures/playground-entry-example-catalog.orna");
const ENTRY_EXAMPLE_CATALOG_ASSET: &str =
    include_str!("fixtures/playground-entry-example-catalog-asset.orna");
const ENTRY_EXAMPLE_CATALOG_SCRIPT: &str =
    include_str!("fixtures/playground-entry-example-catalog-script.orna");
const ENTRY_EXAMPLE_CATALOG_STYLE: &str =
    include_str!("fixtures/playground-entry-example-catalog-style.orna");
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

fn git_stdout(project: &std::path::Path, arguments: &[&str]) -> String {
    let mut command = Command::new("git");
    command.args(arguments).current_dir(project);
    String::from_utf8(run(&mut command, "Git fixture query").stdout)
        .expect("Git output is UTF-8")
        .trim()
        .to_owned()
}

fn write_fixture_rows(project: &std::path::Path, table: &str, rows: &[(&str, &str)]) {
    let directory = project.join("playground").join(table);
    std::fs::create_dir_all(&directory).expect("create playground row directory");
    for (id, source) in rows {
        std::fs::write(directory.join(format!("{id}.orna")), source)
            .expect("write crate-local playground row fixture");
    }
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

fn curl_binary(url: &str) -> Result<(u16, String, Vec<u8>), String> {
    let output = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
    let command = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--noproxy",
            "*",
            "--max-time",
            "15",
            "--output",
        ])
        .arg(output.path())
        .args(["--write-out", "%{http_code}\n%{content_type}"])
        .arg(url)
        .output()
        .map_err(|error| format!("curl: {error}"))?;
    if !command.status.success() {
        return Err(format!(
            "curl exit {}: {}",
            command.status,
            String::from_utf8_lossy(&command.stderr)
        ));
    }
    let metadata = String::from_utf8(command.stdout).map_err(|error| error.to_string())?;
    let (status, content_type) = metadata
        .split_once('\n')
        .ok_or_else(|| "curl response omitted HTTP metadata".to_owned())?;
    let status = status
        .parse::<u16>()
        .map_err(|error| format!("invalid HTTP status: {error}"))?;
    let body = std::fs::read(output.path()).map_err(|error| error.to_string())?;
    Ok((status, content_type.to_owned(), body))
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
    const ROUTES: [(&str, &str); 15] = [
        ("route-2f706c617967726f756e642f", ROUTE_PAGE),
        ("route-2f706c617967726f756e642f656d626564", ROUTE_EMBED),
        (
            "route-2f706c617967726f756e642f6173736574732f6170702e6a73",
            ROUTE_APP,
        ),
        (
            "route-2f706c617967726f756e642f6173736574732f7374796c652e637373",
            ROUTE_STYLE,
        ),
        (
            "route-2f706c617967726f756e642f6173736574732f6f726e612d656469746f722d636f6e6669672e6a736f6e",
            ROUTE_CONFIG,
        ),
        (
            "route-2f706c617967726f756e642f6173736574732f656d6265642e6a73",
            ROUTE_EMBED_SCRIPT,
        ),
        (
            "route-2f706c617967726f756e642f6173736574732f70726573656e746174696f6e2e6d6a73",
            ROUTE_PRESENTATION,
        ),
        (
            "route-2f706c617967726f756e642f6173736574732f73657276652d686f6d652e6d6a73",
            ROUTE_HOME_RUNTIME,
        ),
        (
            "route-2f706c617967726f756e642f6173736574732f73657276652d706c617967726f756e642e6d6a73",
            ROUTE_PLAYGROUND_RUNTIME,
        ),
        (
            "route-2f706c617967726f756e642f6173736574732f6c73702d7761736d2f6f726e615f6c73702e6a73",
            ROUTE_LSP_JS,
        ),
        (
            "route-2f706c617967726f756e642f6173736574732f6c73702d7761736d2f6f726e615f6c73705f62672e7761736d",
            ROUTE_LSP_WASM,
        ),
        (
            "route-2f706c617967726f756e642f6578616d706c65732f",
            ROUTE_EXAMPLE_CATALOG,
        ),
        (
            "route-2f706c617967726f756e642f6173736574732f6578616d706c65732e68746d6c",
            ROUTE_EXAMPLE_CATALOG_ASSET,
        ),
        (
            "route-2f706c617967726f756e642f6173736574732f6578616d706c65732e6d6a73",
            ROUTE_EXAMPLE_CATALOG_SCRIPT,
        ),
        (
            "route-2f706c617967726f756e642f6173736574732f6578616d706c65732e637373",
            ROUTE_EXAMPLE_CATALOG_STYLE,
        ),
    ];
    const ENTRIES: [(&str, &str); 15] = [
        ("entry-page", ENTRY_PAGE),
        ("entry-embed", ENTRY_EMBED),
        ("entry-app", ENTRY_APP),
        ("entry-style", ENTRY_STYLE),
        ("entry-config", ENTRY_CONFIG),
        ("entry-embed-script", ENTRY_EMBED_SCRIPT),
        ("entry-presentation", ENTRY_PRESENTATION),
        ("entry-home-runtime", ENTRY_HOME_RUNTIME),
        ("entry-playground-runtime", ENTRY_PLAYGROUND_RUNTIME),
        ("entry-lsp-js", ENTRY_LSP_JS),
        ("entry-lsp-wasm", ENTRY_LSP_WASM),
        ("entry-example-catalog", ENTRY_EXAMPLE_CATALOG),
        (
            "entry-asset-6173736574732f6578616d706c65732e68746d6c",
            ENTRY_EXAMPLE_CATALOG_ASSET,
        ),
        (
            "entry-asset-6173736574732f6578616d706c65732e6d6a73",
            ENTRY_EXAMPLE_CATALOG_SCRIPT,
        ),
        (
            "entry-asset-6173736574732f6578616d706c65732e637373",
            ENTRY_EXAMPLE_CATALOG_STYLE,
        ),
    ];
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
    write_fixture_rows(
        project.path(),
        "Sample",
        &[
            ("hello", SAMPLE_ROW),
            ("arithmetic", SAMPLE_ARITHMETIC),
            ("functions", SAMPLE_FUNCTIONS),
            ("increment", SAMPLE_INCREMENT),
        ],
    );
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
    std::fs::write(
        assets.join("asset-6173736574732f6f726e612d656469746f722d636f6e6669672e6a736f6e.orna"),
        ASSET_EDITOR_CONFIG,
    )
    .expect("write committed editor configuration fixture");
    std::fs::write(
        assets.join("asset-6173736574732f656d6265642e6a73.orna"),
        ASSET_EMBED,
    )
    .expect("write committed embed entry fixture");
    let themes = project.path().join("playground/Theme");
    std::fs::create_dir_all(&themes).expect("create committed Theme rows");
    std::fs::write(themes.join("wiki-basic.orna"), PLAYGROUND_THEME)
        .expect("write committed Theme fixture");
    let layouts = project.path().join("playground/Layout");
    std::fs::create_dir_all(&layouts).expect("create committed Layout rows");
    std::fs::write(layouts.join("responsive.orna"), PLAYGROUND_LAYOUT)
        .expect("write committed Layout fixture");
    write_fixture_rows(project.path(), "Route", &ROUTES);
    write_fixture_rows(project.path(), "Entry", &ENTRIES);
    for (id, source) in [
        (
            "6173736574732f70726573656e746174696f6e2e6d6a73",
            ASSET_PRESENTATION,
        ),
        ("6173736574732f73657276652d686f6d652e6d6a73", ASSET_HOME),
        (
            "6173736574732f73657276652d706c617967726f756e642e6d6a73",
            ASSET_PLAYGROUND,
        ),
        (
            "6173736574732f6c73702d7761736d2f6f726e615f6c73702e6a73",
            ASSET_LSP_JS,
        ),
        (
            "6173736574732f6c73702d7761736d2f6f726e615f6c73705f62672e7761736d",
            ASSET_LSP_WASM,
        ),
        (
            "6173736574732f6f726e612d656469746f722d636f6e6669672e6a736f6e",
            ASSET_EDITOR_CONFIG,
        ),
        ("6173736574732f656d6265642e6a73", ASSET_EMBED),
        (
            "6173736574732f6578616d706c65732e68746d6c",
            ASSET_EXAMPLE_CATALOG,
        ),
        (
            "6173736574732f6578616d706c65732e6d6a73",
            ASSET_EXAMPLE_CATALOG_SCRIPT,
        ),
        (
            "6173736574732f6578616d706c65732e637373",
            ASSET_EXAMPLE_CATALOG_STYLE,
        ),
    ] {
        std::fs::write(assets.join(format!("asset-{id}.orna")), source)
            .expect("write committed browser support fixture");
    }
    git(
        project.path(),
        &[
            "add",
            "main.orna",
            "playground.orna",
            "playground/Sample",
            "playground/Asset",
            "playground/Route",
            "playground/Entry",
            "playground/Theme",
            "playground/Layout",
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
    assert!(
        page.body
            .contains("href=\"/playground/examples/\">Browse examples</a>")
    );
    assert!(page.body.contains("database shell"));
    assert!(page.body.contains("id=\"live-bridge\""));
    assert!(page.body.contains("id=\"live-presentation\""));
    assert!(page.body.contains("id=\"run-events-source\""));
    assert!(
        page.body
            .contains("src=\"/playground/assets/serve-playground.mjs\"")
    );
    assert!(page.body.contains("orna/serve/run-events/v1"));
    let catalog_page = curl(&format!("{base_url}/playground/examples/"), &[])
        .expect("curl the database-served example catalog page");
    assert_eq!(catalog_page.status, 200);
    assert_eq!(
        response_header(&catalog_page.headers, "content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(catalog_page.body.contains("Orna playground examples"));
    assert!(catalog_page.body.contains("id=\"example-catalog\""));
    assert!(
        catalog_page
            .body
            .contains("/playground/assets/examples.mjs")
    );
    assert!(!catalog_page.body.contains("id=\"live-bridge\""));
    let catalog_script = curl(&format!("{base_url}/playground/assets/examples.mjs"), &[])
        .expect("curl the database-served catalog runtime");
    assert_eq!(catalog_script.status, 200);
    assert!(catalog_script.body.contains("fetch('/api/examples')"));
    let catalog_style = curl(&format!("{base_url}/playground/assets/examples.css"), &[])
        .expect("curl the database-served catalog stylesheet");
    assert_eq!(catalog_style.status, 200);
    assert!(catalog_style.body.contains("body { color: #202122; }"));
    let runtime = curl(
        &format!("{base_url}/playground/assets/serve-playground.mjs"),
        &[],
    )
    .expect("curl the playground live module");
    assert_eq!(runtime.status, 200);
    assert!(runtime.body.contains("globalThis.ornaPlaygroundRun"));
    let presentation_runtime = curl(
        &format!("{base_url}/playground/assets/presentation.mjs"),
        &[],
    )
    .expect("curl the shared presentation runtime");
    assert_eq!(presentation_runtime.status, 200);
    assert!(presentation_runtime.body.contains("class LivePresentation"));
    let home_runtime = curl(&format!("{base_url}/playground/assets/serve-home.mjs"), &[])
        .expect("curl the root live module from the database asset row");
    assert_eq!(home_runtime.status, 200);
    assert!(home_runtime.body.contains("ornaHomeReady"));
    let lsp_binding = curl(
        &format!("{base_url}/playground/assets/lsp-wasm/orna_lsp.js"),
        &[],
    )
    .expect("curl the browser LSP binding from the database asset row");
    assert_eq!(lsp_binding.status, 200);
    assert!(lsp_binding.body.contains("export default"));
    let (status, content_type, lsp_wasm) = curl_binary(&format!(
        "{base_url}/playground/assets/lsp-wasm/orna_lsp_bg.wasm"
    ))
    .expect("curl the browser LSP wasm from the database asset row");
    assert_eq!(status, 200);
    assert_eq!(content_type, "application/wasm");
    assert_eq!(lsp_wasm, b"\0asm\x01\0\0\0");
    let app = curl(&format!("{base_url}/playground/assets/app.js"), &[])
        .expect("curl the playground browser asset");
    assert_eq!(app.status, 200);
    assert!(app.body.contains("ornaPlaygroundReady"));
    let stylesheet = curl(&format!("{base_url}/playground/assets/style.css"), &[])
        .expect("curl the committed database stylesheet");
    assert_eq!(stylesheet.status, 200);
    assert!(stylesheet.body.contains("#202122"));
    let revision_response = curl(&format!("{base_url}/api/playground/revision"), &[])
        .expect("curl the committed playground revision");
    assert_eq!(revision_response.status, 200);
    assert_eq!(
        response_header(&revision_response.headers, "cache-control"),
        Some("no-store")
    );
    let revision_json: JsonValue =
        serde_json::from_str(&revision_response.body).expect("playground revision JSON");
    let initial_revision = revision_json["revision"]
        .as_str()
        .expect("full committed Git revision")
        .to_owned();
    let theme = curl(
        &format!("{base_url}/playground/theme.css?revision={initial_revision}"),
        &[],
    )
    .expect("curl the committed database Theme row");
    assert_eq!(theme.status, 200);
    assert_eq!(
        response_header(&theme.headers, "content-type"),
        Some("text/css; charset=utf-8")
    );
    assert_eq!(theme.body, ":root { --text: #202122; }");
    assert_eq!(
        response_header(&theme.headers, "cache-control"),
        Some("no-cache")
    );
    let layout = curl(
        &format!("{base_url}/playground/layout.css?revision={initial_revision}"),
        &[],
    )
    .expect("curl the committed database Layout row");
    assert_eq!(layout.status, 200);
    assert_eq!(
        response_header(&layout.headers, "content-type"),
        Some("text/css; charset=utf-8")
    );
    assert_eq!(
        layout.body,
        ".workspace { display: grid; } @media (max-width: 64rem) { .workspace { grid-template-columns: 1fr; } }"
    );
    println!(
        "curl Theme/Layout at revision {initial_revision} -> HTTP 200 each (exit 0): committed DB styles"
    );
    let editor_config = curl(
        &format!("{base_url}/playground/assets/orna-editor-config.json"),
        &[],
    )
    .expect("curl the committed editor configuration");
    assert_eq!(editor_config.status, 200);
    let editor_config: JsonValue =
        serde_json::from_str(&editor_config.body).expect("editor configuration JSON");
    assert_eq!(editor_config["language"]["id"], "orna");
    assert_eq!(editor_config["editorOptions"]["tabSize"], 4);
    let embed = curl(&format!("{base_url}/playground/assets/embed.js"), &[])
        .expect("curl the committed embeddable entry script");
    assert_eq!(embed.status, 200);
    assert!(embed.body.contains("new URL(\"../embed\",script.src)"));
    assert!(embed.body.contains("document.createElement(\"iframe\")"));
    assert!(!project.path().join("playground/web-ui/dist").exists());

    let live_route_path = format!("{base_url}/playground/live/");
    let before_route_commit = curl(&live_route_path, &[]).expect("curl missing live route");
    assert_eq!(before_route_commit.status, 404);
    let live_asset_route_path = format!("{base_url}/playground/live-asset/");
    let before_asset_route_commit =
        curl(&live_asset_route_path, &[]).expect("curl missing live asset route");
    assert_eq!(before_asset_route_commit.status, 404);
    write_fixture_rows(
        project.path(),
        "Route",
        &[
            ("route-2f706c617967726f756e642f6c6976652f", ROUTE_LIVE),
            (
                "route-2f706c617967726f756e642f6c6976652d61737365742f",
                ROUTE_LIVE_ASSET,
            ),
        ],
    );
    write_fixture_rows(
        project.path(),
        "Entry",
        &[
            ("entry-live", ENTRY_LIVE),
            ("entry-live-asset", ENTRY_LIVE_ASSET),
        ],
    );
    git(
        project.path(),
        &[
            "add",
            "playground/Route/route-2f706c617967726f756e642f6c6976652f.orna",
            "playground/Route/route-2f706c617967726f756e642f6c6976652d61737365742f.orna",
            "playground/Entry/entry-live.orna",
            "playground/Entry/entry-live-asset.orna",
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
            "add live playground route records",
        ],
    );
    assert!(
        server
            .0
            .try_wait()
            .expect("probe orna serve process")
            .is_none()
    );
    let after_route_commit = curl(&live_route_path, &[]).expect("curl newly committed live route");
    assert_eq!(after_route_commit.status, 200);
    assert_eq!(
        response_header(&after_route_commit.headers, "content-type"),
        Some("text/html; charset=utf-8")
    );
    assert!(after_route_commit.body.contains("database shell"));

    assert!(
        server
            .0
            .try_wait()
            .expect("probe orna serve process after route bindings")
            .is_none()
    );
    let live_asset =
        curl(&live_asset_route_path, &[]).expect("curl newly committed live asset route");
    assert_eq!(live_asset.status, 200);
    assert_eq!(
        response_header(&live_asset.headers, "content-type"),
        Some("text/javascript; charset=utf-8")
    );
    assert_eq!(live_asset.body, "globalThis.ornaPlaygroundReady = true;");
    println!(
        "live DB route and entry reload: GET /playground/live/ before route commit -> HTTP {} and after page entry -> HTTP {}; GET /playground/live-asset/ before route commit -> HTTP {} and after asset entry -> HTTP {} ({}) without restarting orna serve (exit 0)",
        before_route_commit.status,
        after_route_commit.status,
        before_asset_route_commit.status,
        live_asset.status,
        response_header(&live_asset.headers, "content-type").unwrap_or("missing"),
    );

    let route_revision_response = curl(&format!("{base_url}/api/playground/revision"), &[])
        .expect("curl the revision after the route commit");
    assert_eq!(route_revision_response.status, 200);
    let route_revision_json: JsonValue =
        serde_json::from_str(&route_revision_response.body).expect("route revision JSON");
    let route_revision = route_revision_json["revision"]
        .as_str()
        .expect("committed route revision")
        .to_owned();
    std::fs::write(themes.join("wiki-basic.orna"), PLAYGROUND_THEME_UPDATED)
        .expect("write uncommitted Theme change");
    std::fs::write(layouts.join("responsive.orna"), PLAYGROUND_LAYOUT_UPDATED)
        .expect("write uncommitted Layout change");
    let uncommitted_revision_response = curl(&format!("{base_url}/api/playground/revision"), &[])
        .expect("curl revision while style edits are uncommitted");
    let uncommitted_revision_json: JsonValue =
        serde_json::from_str(&uncommitted_revision_response.body)
            .expect("uncommitted revision JSON");
    assert_eq!(
        uncommitted_revision_json["revision"].as_str(),
        Some(route_revision.as_str())
    );
    let uncommitted_theme = curl(&format!("{base_url}/playground/theme.css"), &[])
        .expect("curl Theme while the changed row is uncommitted");
    let uncommitted_layout = curl(&format!("{base_url}/playground/layout.css"), &[])
        .expect("curl Layout while the changed row is uncommitted");
    assert_eq!(uncommitted_theme.status, 200);
    assert_eq!(uncommitted_theme.body, ":root { --text: #202122; }");
    assert_eq!(uncommitted_layout.status, 200);
    assert_eq!(
        uncommitted_layout.body,
        ".workspace { display: grid; } @media (max-width: 64rem) { .workspace { grid-template-columns: 1fr; } }"
    );
    git(
        project.path(),
        &[
            "add",
            "playground/Theme/wiki-basic.orna",
            "playground/Layout/responsive.orna",
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
            "update playground Theme and Layout records",
        ],
    );
    let updated_revision_response = curl(&format!("{base_url}/api/playground/revision"), &[])
        .expect("curl revision after the Theme and Layout commit");
    let updated_revision_json: JsonValue =
        serde_json::from_str(&updated_revision_response.body).expect("updated revision JSON");
    let updated_revision = updated_revision_json["revision"]
        .as_str()
        .expect("committed Theme and Layout revision");
    assert_ne!(updated_revision, route_revision);
    let updated_theme = curl(&format!("{base_url}/playground/theme.css"), &[])
        .expect("curl committed updated Theme row");
    let updated_layout = curl(&format!("{base_url}/playground/layout.css"), &[])
        .expect("curl committed updated Layout row");
    assert_eq!(updated_theme.status, 200);
    assert_eq!(updated_theme.body, ":root { --text: #111111; }");
    assert_eq!(updated_layout.status, 200);
    assert_eq!(
        updated_layout.body,
        ".workspace { display: grid; grid-template-columns: 1fr 1fr; } @media (max-width: 64rem) { .workspace { grid-template-columns: 1fr; } }"
    );
    let pinned_theme = curl(
        &format!("{base_url}/playground/theme.css?revision={route_revision}"),
        &[],
    )
    .expect("curl Theme pinned to the earlier revision");
    let pinned_layout = curl(
        &format!("{base_url}/playground/layout.css?revision={route_revision}"),
        &[],
    )
    .expect("curl Layout pinned to the earlier revision");
    assert_eq!(pinned_theme.status, 200);
    assert_eq!(pinned_theme.body, ":root { --text: #202122; }");
    assert_eq!(pinned_layout.status, 200);
    assert_eq!(
        pinned_layout.body,
        ".workspace { display: grid; } @media (max-width: 64rem) { .workspace { grid-template-columns: 1fr; } }"
    );
    assert!(
        server
            .0
            .try_wait()
            .expect("probe orna serve after the style commit")
            .is_none()
    );
    println!(
        "live DB Theme/Layout reload: uncommitted styles stayed at revision {}; committed revision {} served both updates and pinned old rows (exit 0)",
        route_revision, updated_revision
    );

    let examples_started = Instant::now();
    let examples =
        curl(&format!("{base_url}/api/examples"), &["--max-time", "15"]).unwrap_or_else(|error| {
            let server_status = server.0.try_wait().expect("probe orna serve process");
            panic!("curl committed playground examples: {error}; server status {server_status:?}");
        });
    println!(
        "curl GET /api/examples -> HTTP {} (exit 0, {:?})",
        examples.status,
        examples_started.elapsed()
    );
    assert_eq!(examples.status, 200);
    let examples: JsonValue = serde_json::from_str(&examples.body).expect("example response JSON");
    let revision = examples["revision"]
        .as_str()
        .expect("committed API revision");
    assert_eq!(revision.len(), 40);
    assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
    let initial_examples_revision = revision.to_owned();
    let examples = examples["examples"]
        .as_array()
        .expect("committed example catalog");
    assert_eq!(examples.len(), 4);
    assert!(
        examples
            .iter()
            .all(|example| { example["path"] != "playground/Sample/live-after-start.orna" })
    );
    let arithmetic = examples
        .iter()
        .find(|example| example["path"] == "playground/Sample/arithmetic.orna")
        .expect("arithmetic sample row");
    assert_eq!(arithmetic["name"], "Arithmetic");
    assert_eq!(arithmetic["source"], "6 * 7");
    let functions = examples
        .iter()
        .find(|example| example["path"] == "playground/Sample/functions.orna")
        .expect("function sample row");
    assert_eq!(functions["name"], "Function composition");
    let increment = examples
        .iter()
        .find(|example| example["path"] == "playground/Sample/increment.orna")
        .expect("standard library sample row");
    assert_eq!(
        increment["source"],
        "use std.math.{increment};\nincrement(41)"
    );
    let hello = examples
        .iter()
        .find(|example| example["path"] == "playground/Sample/hello.orna")
        .expect("hello sample row");
    assert_eq!(
        hello["source"],
        "pub fn double(value: Int): Int = value + value;\ndouble(21)"
    );
    let initial_sample_source = hello["source"]
        .as_str()
        .expect("initial sample source")
        .to_owned();
    let record_page = curl(
        &format!("{base_url}/blob/{revision}/playground/Sample/arithmetic.orna"),
        &[],
    )
    .expect("curl the committed sample source listing page");
    assert_eq!(record_page.status, 200);
    assert!(record_page.body.contains("Arithmetic"));
    assert!(record_page.body.contains("6 * 7"));

    write_fixture_rows(
        project.path(),
        "Sample",
        &[("live-after-start", LIVE_PROGRAM_ROW)],
    );
    let uncommitted_programs = curl(&format!("{base_url}/api/examples"), &["--max-time", "15"])
        .expect("curl the catalog while the new program row is uncommitted");
    assert_eq!(uncommitted_programs.status, 200);
    let uncommitted_programs: JsonValue =
        serde_json::from_str(&uncommitted_programs.body).expect("uncommitted example catalog JSON");
    let uncommitted_program_list = uncommitted_programs["examples"]
        .as_array()
        .expect("uncommitted committed example catalog");
    assert_eq!(uncommitted_program_list.len(), 4);
    assert!(
        uncommitted_program_list
            .iter()
            .all(|example| { example["path"] != "playground/Sample/live-after-start.orna" })
    );
    println!(
        "uncommitted DB program stayed out of /api/examples (HTTP {}, 4 committed rows, exit 0)",
        uncommitted_programs["examples"].as_array().unwrap().len()
    );
    git(
        project.path(),
        &["add", "playground/Sample/live-after-start.orna"],
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
            "add live playground program",
        ],
    );
    let live_examples = curl(&format!("{base_url}/api/examples"), &["--max-time", "15"])
        .expect("curl the live database program catalog");
    assert_eq!(live_examples.status, 200);
    let live_examples: JsonValue =
        serde_json::from_str(&live_examples.body).expect("updated example catalog JSON");
    let live_example_list = live_examples["examples"]
        .as_array()
        .expect("updated committed example catalog");
    assert_eq!(live_example_list.len(), 5);
    assert_ne!(
        live_examples["revision"].as_str(),
        Some(initial_examples_revision.as_str())
    );
    let live_program = live_example_list
        .iter()
        .find(|example| example["path"] == "playground/Sample/live-after-start.orna")
        .expect("program committed while orna serve was running");
    assert_eq!(live_program["name"], "Live after startup");
    assert_eq!(live_program["source"], "6 * 7");
    let live_program_source = live_program["source"]
        .as_str()
        .expect("database-resident program source")
        .to_owned();
    assert_eq!(live_program_source, "6 * 7");
    println!(
        "live DB program catalog: /api/examples absent before commit, present afterward as {} (HTTP {}, exit 0)",
        live_program["path"],
        live_examples["examples"].as_array().unwrap().len()
    );

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

    let start = Arc::new(Barrier::new(4));
    let mut readers = Vec::new();
    for _ in 0..2 {
        let start = Arc::clone(&start);
        let base_url = base_url.clone();
        readers.push(thread::spawn(move || {
            start.wait();
            let asset = curl(
                &format!("{base_url}/playground/assets/app.js"),
                &["--max-time", "15"],
            )
            .expect("curl a DB asset during concurrent commits");
            let examples = curl(&format!("{base_url}/api/examples"), &["--max-time", "15"])
                .expect("curl DB examples during concurrent commits");
            (asset, examples)
        }));
    }
    let writer_root = project.path().to_path_buf();
    let writer_start = Arc::clone(&start);
    let writer = thread::spawn(move || {
        writer_start.wait();
        let mut snapshots = BTreeMap::new();
        for generation in 1..=4 {
            let entry_id = format!("entry-live-snapshot-{generation}");
            let asset_path = format!("assets/app-live-snapshot-{generation}.js");
            let asset_id = format!(
                "asset-{}",
                asset_path
                    .bytes()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            );
            let previous_entry_id = if generation == 1 {
                "entry-app".to_owned()
            } else {
                format!("entry-live-snapshot-{}", generation - 1)
            };
            let previous_asset_id = if generation == 1 {
                "asset-6173736574732f6170702e6a73".to_owned()
            } else {
                let previous_path = format!("assets/app-live-snapshot-{}.js", generation - 1);
                format!(
                    "asset-{}",
                    previous_path
                        .bytes()
                        .map(|byte| format!("{byte:02x}"))
                        .collect::<String>()
                )
            };
            let route = LIVE_SNAPSHOT_ROUTE_TEMPLATE.replace("entry-live-snapshot", &entry_id);
            let entry = LIVE_SNAPSHOT_ENTRY_TEMPLATE
                .replace("entry-live-snapshot", &entry_id)
                .replace("assets/app-live-snapshot.js", &asset_path);
            let asset = LIVE_SNAPSHOT_ASSET_TEMPLATE
                .replace("asset-live-snapshot", &asset_id)
                .replace("assets/app-live-snapshot.js", &asset_path)
                .replace("Snapshot = 1;", &format!("Snapshot = {generation};"));
            let sample = LIVE_SNAPSHOT_SAMPLE_TEMPLATE
                .replace("Live snapshot 1", &format!("Live snapshot {generation}"))
                .replace("snapshot-1", &format!("snapshot-{generation}"));
            write_fixture_rows(
                &writer_root,
                "Route",
                &[(
                    "route-2f706c617967726f756e642f6173736574732f6170702e6a73",
                    &route,
                )],
            );
            std::fs::remove_file(
                writer_root
                    .join("playground/Entry")
                    .join(format!("{previous_entry_id}.orna")),
            )
            .expect("remove prior entry from the new commit");
            write_fixture_rows(&writer_root, "Entry", &[(&entry_id, &entry)]);
            std::fs::remove_file(
                writer_root
                    .join("playground/Asset")
                    .join(format!("{previous_asset_id}.orna")),
            )
            .expect("remove prior asset from the new commit");
            write_fixture_rows(&writer_root, "Asset", &[(&asset_id, &asset)]);
            write_fixture_rows(&writer_root, "Sample", &[("hello", &sample)]);
            git(
                &writer_root,
                &[
                    "add",
                    "-A",
                    "playground/Route",
                    "playground/Entry",
                    "playground/Asset",
                    "playground/Sample/hello.orna",
                ],
            );
            let message = format!("advance playground snapshot {generation}");
            git(
                &writer_root,
                &[
                    "-c",
                    "user.name=kierandrewett",
                    "-c",
                    "user.email=kieran@drewett.dev",
                    "commit",
                    "--quiet",
                    "-m",
                    &message,
                ],
            );
            let revision = git_stdout(&writer_root, &["rev-parse", "HEAD"]);
            snapshots.insert(revision, format!("snapshot-{generation}"));
            thread::sleep(Duration::from_millis(10));
        }
        snapshots
    });
    start.wait();

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

    let mut snapshots = writer.join().expect("join live DB snapshot writer");
    snapshots.insert(initial_examples_revision, initial_sample_source);
    let mut asset_responses = 0;
    let mut example_responses = 0;
    for reader in readers {
        let (asset, examples) = reader.join().expect("join concurrent DB reader");
        assert_eq!(asset.status, 200);
        assert!(
            asset
                .body
                .contains("globalThis.ornaPlaygroundReady = true;")
                || (1..=4).any(|generation| {
                    asset.body.contains(&format!(
                        "globalThis.ornaPlaygroundSnapshot = {generation};"
                    ))
                }),
            "asset response must come from a committed snapshot: {}",
            asset.body
        );
        asset_responses += 1;

        assert_eq!(examples.status, 200);
        let examples: JsonValue =
            serde_json::from_str(&examples.body).expect("live examples response JSON");
        let revision = examples["revision"]
            .as_str()
            .expect("live examples revision");
        let source = examples["examples"]
            .as_array()
            .and_then(|examples| {
                examples
                    .iter()
                    .find(|example| example["path"] == "playground/Sample/hello.orna")
            })
            .and_then(|example| example["source"].as_str())
            .expect("live hello sample");
        assert_eq!(
            snapshots.get(revision).map(String::as_str),
            Some(source),
            "the example body must match its committed revision"
        );
        example_responses += 1;
    }
    println!(
        "concurrent GET /playground/assets/app.js x{asset_responses} (HTTP 200, exit 0): Route, Entry, and Asset rows stay snapshot-correct across live commits"
    );
    println!(
        "concurrent GET /api/examples x{example_responses} (HTTP 200, exit 0): each Sample body matches its response revision during live deltas"
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

    let live_program_eval_request = [0x38; 16];
    let live_program_result = send_and_read_request(
        &mut socket,
        evaluation(
            session_bytes,
            uuid_bytes(database_id),
            live_program_eval_request,
            &live_program_source,
        ),
        live_program_eval_request,
    );
    assert!(matches!(
        live_program_result.message,
        Message::Result {
            status: ResultStatus::Success,
            value: Some(value),
            ..
        } if value.raw() == &OvbRaw::Int(42.into())
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
        "curl GET /playground/assets/orna-editor-config.json -> HTTP {} (exit 0): DB-resident editor configuration",
        200
    );
    println!(
        "curl GET /playground/assets/embed.js -> HTTP {} (exit 0): DB-resident iframe entry script",
        embed.status
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
    println!("WebSocket EVAL exact source fetched from live /api/examples -> Result 42 (exit 0)");
}
