//! The executable edge for `orna serve`.
//!
//! The reference requires a loopback default but does not prescribe URL
//! paths, so this host provides a small stable surface: a landing page,
//! read-only clone/page routes, a pure expression query, and the existing
//! authenticated `orna.present.v1` session/WebSocket routes.

use super::*;
use orna_application_v1::ApplicationLiveAdapter;
use orna_live_v1::{
    HttpConnection, LiveHost, LiveSessionAuthority, LiveTransport,
    SessionMetadata, SystemCredentialIssuer, TransportLimits, WireRequest,
};
use orna_protocol_v1::{
    Envelope, Limits as ProtocolLimits, Message, PresentationContext,
};
use orna_repository_v1::{Repository, RuntimeGeneration};
use orna_security_v1::{Origin, OriginPolicy, SessionBoundary, SessionDeletionAdapter};
use orna_serving_v1::Serving;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    time::{Duration as StdDuration, SystemTime, UNIX_EPOCH},
};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;
const SESSION_LEASE_MS: u64 = 60 * 60 * 1000;

struct ServeState {
    root: PathBuf,
    identity: RuntimeIdentity,
    live: LiveTransport,
    authority: LocalSessionAuthority,
    issuer: SystemCredentialIssuer,
    deletion: LocalSessionDeletion,
    application: ApplicationLiveAdapter,
}

struct LocalSessionAuthority {
    identity: RuntimeIdentity,
}

impl LiveSessionAuthority for LocalSessionAuthority {
    fn create_session(
        &mut self,
        database: [u8; 16],
        now: u64,
    ) -> Result<SessionMetadata, orna_live_v1::Error> {
        if database != self.identity.database_id {
            return Err(orna_live_v1::Error::Denied);
        }
        let mut session = [0; 16];
        getrandom::fill(&mut session).map_err(|_| orna_live_v1::Error::RuntimeUnavailable)?;
        let subscription = Envelope {
            request: Some([1; 16]),
            watch: None,
            message: Message::Subscribe {
                resource: [2; 16],
                presentation: PresentationContext {
                    locale: "en".into(),
                    timezone: None,
                    width: None,
                    theme: "system".into(),
                    supported_kinds: vec!["text".into(), "group".into()],
                },
            },
            extensions: BTreeMap::new(),
        }
        .encode(ProtocolLimits::default())
        .map_err(|_| orna_live_v1::Error::RuntimeUnavailable)?;
        Ok(SessionMetadata {
            session,
            database,
            runtime: self.identity.repository_id,
            expires_at: now.saturating_add(SESSION_LEASE_MS),
            subscribe: subscription,
        })
    }
}

#[derive(Default)]
struct LocalSessionDeletion;

impl SessionDeletionAdapter for LocalSessionDeletion {
    type Error = ();

    fn delete(&mut self, _: orna_security_v1::SessionId) -> Result<(), Self::Error> {
        // Live sessions are process-local in this host, so there is no
        // durable session record to delete.
        Ok(())
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct Response {
    status: u16,
    content_type: String,
    body: Vec<u8>,
}

impl Response {
    fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type: content_type.into(),
            body: body.into(),
        }
    }
}

pub(super) fn run(endpoint: &Endpoint, port: u16) -> Result<(), Diagnostic> {
    let root = local_project_path(endpoint)?;
    let repository = Repository::discover(&root).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run `serve` inside an Orna Git worktree or provide a local project path",
        )
    })?;

    // Project loading parses the reachable source modules only. In
    // particular, serving a clone never invokes its root `main()`; functions
    // run only after an explicit query or live Eval request reaches a route.
    let _project = load_project(endpoint)?;
    let (identity, _) = runtime_identity(&repository)?;
    let listener = LiveTransport::bind_default_listener(port).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "the loopback serving listener could not be bound",
            "choose a free port with `serve --port PORT`, then retry",
        )
    })?;
    let address = listener.status().address;
    let state = new_serve_state(repository.worktree().to_path_buf(), identity, address)?;
    writeln!(
        io::stdout().lock(),
        "Serving {} at http://{} (loopback)",
        state.root.display(),
        address
    )
    .map_err(|_| {
        Diagnostic::target(
            "E2200",
            "serving status could not be written",
            "check the output stream, then start `serve` again",
        )
    })?;

    serve_listener(listener.listener(), state).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "the serving listener stopped",
            "check local socket access, then retry `serve`",
        )
    })
}

fn new_serve_state(
    root: PathBuf,
    identity: RuntimeIdentity,
    listener_address: SocketAddr,
) -> Result<ServeState, Diagnostic> {
    let origin = Origin::parse(&format!("http://{listener_address}")).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "the loopback serving origin is invalid",
            "retry `serve` with a supported local listener",
        )
    })?;
    let boundary = SessionBoundary::new(OriginPolicy::new([origin], []), SESSION_LEASE_MS);
    let host = LiveHost::new(
        orna_live_v1::Limits::default(),
        boundary,
        Serving::new(orna_serving_v1::Limits::default()).map_err(|_| {
            Diagnostic::target(
                "E2100",
                "serving limits could not be initialized",
                "retry `serve`; the local serving state is unavailable",
            )
        })?,
    )
    .map_err(|_| {
        Diagnostic::target(
            "E2100",
            "the live protocol host could not be initialized",
            "retry `serve`; the local serving state is unavailable",
        )
    })?;
    let live = LiveTransport::new(host, TransportLimits::default()).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "the live transport could not be initialized",
            "retry `serve`; the local serving state is unavailable",
        )
    })?;
    let application = ApplicationLiveAdapter::new(ApplicationAuthority::new(
        semantic_catalogue(),
        Limits::default(),
    ));
    Ok(ServeState {
        root,
        identity,
        live,
        authority: LocalSessionAuthority { identity },
        issuer: SystemCredentialIssuer::default(),
        deletion: LocalSessionDeletion,
        application,
    })
}

fn serve_listener(listener: &TcpListener, mut state: ServeState) -> io::Result<()> {
    for accepted in listener.incoming() {
        let stream = accepted?;
        // The live transport's protocol state is mutable and currently
        // exposes a synchronous connection driver, so this first executable
        // host serializes accepted connections. Static and Git routes remain
        // ordinary short HTTP requests; a future async listener actor can
        // multiplex them without changing their route contract.
        let _ = serve_connection(stream, &mut state);
    }
    Ok(())
}

fn serve_connection(mut stream: TcpStream, state: &mut ServeState) -> io::Result<()> {
    stream.set_read_timeout(Some(StdDuration::from_secs(15)))?;
    if peek_websocket_request(&stream) {
        stream.set_read_timeout(None)?;
        let mut attachment = [0; 16];
        getrandom::fill(&mut attachment)
            .map_err(|_| io::Error::other("socket identity unavailable"))?;
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut clock = now_ms;
        let _ = state.live.serve_accepted_websocket_socket(
            stream,
            &mut connection,
            attachment,
            &mut clock,
            &mut state.application,
        );
        return Ok(());
    }

    let mut reader = BufReader::new(stream);
    let request = match read_request(&mut reader) {
        Ok(request) => request,
        Err(_) => {
            let mut stream = reader.into_inner();
            write_response(
                &mut stream,
                Response::new(400, "application/json", br#"{"error":"bad_request"}"#.to_vec()),
            )?;
            return Ok(());
        }
    };
    stream = reader.into_inner();
    if request.path.starts_with("/orna/session") {
        let wire = WireRequest {
            method: request.method,
            path: request.path,
            headers: request.headers,
            body: request.body,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|_| io::Error::other("live request executor unavailable"))?;
        let response = runtime.block_on(state.live.handle(
            wire,
            now_ms(),
            &mut state.authority,
            &mut state.issuer,
            &mut state.deletion,
        ));
        let bytes = response
            .encode_http(TransportLimits::default())
            .map_err(|_| io::Error::other("live response could not be encoded"))?;
        stream.write_all(&bytes)?;
        return Ok(());
    }
    let response = host_route(&state.root, state.identity, &request);
    write_response(&mut stream, response)
}

// This slice wires clone metadata, project source pages, pure queries, and
// authenticated live sessions. The full browser frontend and Git transport
// remain implemented-later work; the reference leaves their concrete routes
// open, so this host does not present placeholder Git URLs as working routes.
fn host_route(root: &Path, identity: RuntimeIdentity, request: &Request) -> Response {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => Response::new(200, "text/html; charset=utf-8", LANDING_PAGE),
        ("GET", "/api/clone") => match clone_report(root, identity) {
            Ok(report) => Response::new(200, "application/json", report),
            Err(_) => unavailable_response(),
        },
        ("GET", "/api/pages") => match load_current_project(root) {
            Ok(project) => {
                let mut pages = String::from("[");
                for (index, identity) in project.identities().iter().enumerate() {
                    if index > 0 {
                        pages.push(',');
                    }
                    pages.push_str(&format!(
                        "{{\"path\":{},\"source\":{}}}",
                        json_string(&format!("/pages/{}", identity.logical_path())),
                        json_string(identity.logical_path()),
                    ));
                }
                pages.push(']');
                Response::new(200, "application/json", pages.into_bytes())
            }
            Err(_) => unavailable_response(),
        },
        ("GET", path) if path.starts_with("/pages/") => {
            let requested = &path[7..];
            match load_current_project(root) {
                Ok(project) => project
                    .identities()
                    .iter()
                    .position(|identity| identity.logical_path() == requested)
                    .and_then(|index| project.modules().get(index))
                    .map_or_else(
                        || not_found_response(),
                        |module| Response::new(200, "text/plain; charset=utf-8", module.source.clone()),
                    ),
                Err(_) => unavailable_response(),
            }
        }
        ("POST", "/api/query") => match std::str::from_utf8(&request.body) {
            Ok(source) => orna_evaluator_v1::evaluate_repl(
                source,
                &Environment::new(),
                Limits::default(),
            )
            .map_or_else(
                |error| {
                    Response::new(
                        422,
                        "application/json",
                        format!("{{\"error\":{}}}", json_string(error.code())),
                    )
                },
                |value| Response::new(200, "text/plain; charset=utf-8", format!("{value:?}")),
            ),
            Err(_) => Response::new(400, "application/json", br#"{"error":"invalid_utf8"}"#.to_vec()),
        },
        _ => not_found_response(),
    }
}

fn load_current_project(root: &Path) -> Result<orna_project_v1::LoadedProject, ()> {
    let repository = Repository::discover(root).map_err(|_| ())?;
    orna_project_v1::ProjectLoader::default()
        .load(&repository)
        .map_err(|_| ())
}

fn clone_report(root: &Path, identity: RuntimeIdentity) -> io::Result<Vec<u8>> {
    let repository = Repository::discover(root)
        .map_err(|_| io::Error::other("clone is unavailable"))?;
    let head = repository
        .head()
        .map_err(|_| io::Error::other("clone HEAD is unavailable"))?;
    let cwd = repository
        .cwd_generation(RuntimeGeneration::new(0))
        .map_err(|_| io::Error::other("clone CWD is unavailable"))?;
    let head_json = head
        .as_ref()
        .map_or_else(|| "null".to_owned(), |head| json_string(head.as_str()));
    let branch_json = cwd
        .branch()
        .map_or_else(|| "null".to_owned(), json_string);
    let index_tree_json = cwd
        .index()
        .tree()
        .map_or_else(|| "null".to_owned(), |tree| json_string(tree.as_str()));
    let report = format!(
        "{{\"clone\":{},\"database\":{},\"runtime\":{},\"HEAD\":{},\"CWD\":{{\"branch\":{},\"index_tree\":{},\"worktree_clean\":{}}}}}",
        json_string(&repository.worktree().to_string_lossy()),
        json_string(&format_uuid(identity.database_id)),
        json_string(&format_uuid(identity.repository_id)),
        head_json,
        branch_json,
        index_tree_json,
        cwd.worktree().is_clean(),
    );
    Ok(report.into_bytes())
}

fn read_request(reader: &mut BufReader<TcpStream>) -> io::Result<Request> {
    let mut header_bytes = 0usize;
    let mut line = Vec::new();
    reader.read_until(b'\n', &mut line)?;
    header_bytes = header_bytes.saturating_add(line.len());
    let request_line = std::str::from_utf8(&line)
        .map_err(|_| io::Error::other("invalid request line"))?
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    let mut parts = request_line.split(' ');
    let method = parts
        .next()
        .ok_or_else(|| io::Error::other("bad request"))?
        .to_owned();
    let target = parts
        .next()
        .ok_or_else(|| io::Error::other("bad request"))?
        .to_owned();
    let version = parts
        .next()
        .ok_or_else(|| io::Error::other("bad request"))?;
    if parts.next().is_some()
        || !matches!(method.as_str(), "GET" | "POST" | "DELETE")
        || version != "HTTP/1.1"
        || !target.starts_with('/')
        || target.starts_with("//")
        || target.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
    {
        return Err(io::Error::other("bad request"));
    }
    let mut headers = Vec::new();
    let mut content_length = None;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Err(io::Error::other("incomplete request"));
        }
        header_bytes = header_bytes.saturating_add(line.len());
        if header_bytes > MAX_HEADER_BYTES {
            return Err(io::Error::other("request header limit"));
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        let text = std::str::from_utf8(&line)
            .map_err(|_| io::Error::other("invalid request header"))?
            .trim_end_matches(['\r', '\n']);
        let (name, value) = text
            .split_once(':')
            .ok_or_else(|| io::Error::other("invalid request header"))?;
        if name.is_empty()
            || name.bytes().any(|byte| !byte.is_ascii_alphanumeric() && byte != b'-')
            || value.contains(['\r', '\n'])
            || headers
                .iter()
                .any(|(current, _): &(String, String)| current.eq_ignore_ascii_case(name))
        {
            return Err(io::Error::other("invalid request header"));
        }
        let value = value.trim().to_owned();
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(io::Error::other("unsupported transfer encoding"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            if !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(io::Error::other("invalid content length"));
            }
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| io::Error::other("invalid content length"))?,
            );
        }
        headers.push((name.to_owned(), value));
    }
    if !headers
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("host"))
    {
        return Err(io::Error::other("missing Host header"));
    }
    let body_length = content_length.unwrap_or(0);
    if body_length > MAX_REQUEST_BODY_BYTES {
        return Err(io::Error::other("request body limit"));
    }
    let mut body = vec![0; body_length];
    reader.read_exact(&mut body)?;
    let path = target.split_once('?').map_or(target.as_str(), |(path, _)| path);
    Ok(Request {
        method,
        path: path.to_owned(),
        headers,
        body,
    })
}

fn peek_websocket_request(stream: &TcpStream) -> bool {
    let mut bytes = [0; MAX_HEADER_BYTES];
    loop {
        let Ok(count) = stream.peek(&mut bytes) else {
            return false;
        };
        if count == 0 || count == bytes.len() {
            return false;
        }
        let Ok(text) = std::str::from_utf8(&bytes[..count]) else {
            return false;
        };
        let Some(end) = text.find('\n') else {
            // TCP may split even the request line. Peek again without
            // consuming it so the protocol driver receives the full upgrade.
            std::thread::sleep(StdDuration::from_millis(2));
            continue;
        };
        let mut fields = text[..end].trim_end_matches('\r').split(' ');
        return fields.next() == Some("GET")
            && fields
                .next()
                .is_some_and(|path| path.starts_with("/orna/live/"));
    }
}

fn write_response(stream: &mut TcpStream, response: Response) -> io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Payload Too Large",
        422 => "Unprocessable Content",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Response",
    };
    write!(stream, "HTTP/1.1 {} {reason}\r\n", response.status)?;
    if !response.content_type.is_empty() {
        write!(stream, "Content-Type: {}\r\n", response.content_type)?;
    }
    write!(
        stream,
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        response.body.len()
    )?;
    stream.write_all(&response.body)?;
    stream.flush()
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len().saturating_add(2));
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(output, "\\u{:04x}", u32::from(character));
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

fn format_uuid(bytes: [u8; 16]) -> String {
    let mut output = String::with_capacity(36);
    for (index, byte) in bytes.into_iter().enumerate() {
        if [4, 6, 8, 10].contains(&index) {
            output.push('-');
        }
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

fn not_found_response() -> Response {
    Response::new(404, "application/json", br#"{"error":"not_found"}"#.to_vec())
}

fn unavailable_response() -> Response {
    Response::new(
        503,
        "application/json",
        br#"{"error":"temporarily_unavailable"}"#.to_vec(),
    )
}

const LANDING_PAGE: &str = r#"<!doctype html>
<html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>Orna</title><main><h1>Orna</h1><p>This page is served from the current clone.</p>
<p><a href="/api/clone">Clone status</a> · <a href="/api/pages">Project modules</a></p>
<form id="query"><label>Pure Orna expression <input name="source" value="1 + 1"></label><button>Evaluate</button></form><pre id="result"></pre>
<script>document.querySelector('#query').addEventListener('submit',async e=>{e.preventDefault();let r=await fetch('/api/query',{method:'POST',body:new FormData(e.target).get('source')});document.querySelector('#result').textContent=await r.text()})</script>
</main></html>"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const SERVE_FIXTURE: &str = include_str!("../tests/fixtures/serve-no-autoload.orna");

    #[test]
    fn serving_loads_the_local_main_module_without_evaluating_it() {
        let directory = tempfile::tempdir().expect("temporary clone");
        std::fs::write(directory.path().join("main.orna"), SERVE_FIXTURE)
            .expect("vendored main module");
        assert!(
            Command::new("git")
                .args(["init", "--quiet", "--initial-branch", "serve-test"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );
        let repository = Repository::discover(directory.path()).expect("clone");
        let loaded = orna_project_v1::ProjectLoader::default()
            .load(&repository)
            .expect("loading parses but does not run main");
        assert_eq!(loaded.identities()[0].logical_path(), "main.orna");
        assert!(loaded.modules()[0].source.contains("1 / 0"));

        let identity = RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        };
        let pages = host_route(
            directory.path(),
            identity,
            &Request {
                method: "GET".into(),
                path: "/api/pages".into(),
                headers: Vec::new(),
                body: Vec::new(),
            },
        );
        assert_eq!(pages.status, 200);
        assert!(String::from_utf8(pages.body).unwrap().contains("/pages/main.orna"));
        let query = host_route(
            directory.path(),
            identity,
            &Request {
                method: "POST".into(),
                path: "/api/query".into(),
                headers: Vec::new(),
                body: b"1 + 1".to_vec(),
            },
        );
        assert_eq!(query.status, 200);
        assert_eq!(query.body, b"Value(Int(2))");
    }

    #[test]
    fn clone_route_reports_head_and_cwd_from_the_selected_clone() {
        let left = tempfile::tempdir().expect("left clone");
        let right = tempfile::tempdir().expect("right clone");
        for (directory, branch) in [(&left, "clone-left"), (&right, "clone-right")] {
            assert!(
                Command::new("git")
                    .args(["init", "--quiet", "--initial-branch", branch])
                    .current_dir(directory.path())
                    .status()
                    .expect("git")
                    .success()
            );
            assert!(
                Command::new("git")
                    .args([
                        "-c",
                        "user.name=kierandrewett",
                        "-c",
                        "user.email=kieran@drewett.dev",
                        "commit",
                        "--quiet",
                        "--allow-empty",
                        "-m",
                        branch,
                    ])
                    .current_dir(directory.path())
                    .status()
                    .expect("git commit")
                    .success()
            );
        }
        std::fs::write(right.path().join("local.txt"), "only in right")
            .expect("right CWD change");
        let left_report = String::from_utf8(
            clone_report(left.path(), RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            })
            .expect("left report"),
        )
        .expect("left JSON");
        let right_report = String::from_utf8(
            clone_report(right.path(), RuntimeIdentity {
                database_id: [3; 16],
                repository_id: [4; 16],
            })
            .expect("right report"),
        )
        .expect("right JSON");
        let left_head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(left.path())
            .output()
            .expect("left HEAD")
            .stdout;
        let right_head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(right.path())
            .output()
            .expect("right HEAD")
            .stdout;
        let left_head = String::from_utf8(left_head).expect("left HEAD UTF-8");
        let right_head = String::from_utf8(right_head).expect("right HEAD UTF-8");
        assert!(left_report.contains("clone-left"));
        assert!(left_report.contains("\"worktree_clean\":true"));
        assert!(right_report.contains("clone-right"));
        assert!(right_report.contains("\"worktree_clean\":false"));
        assert!(left_report.contains(left_head.trim()));
        assert!(right_report.contains(right_head.trim()));
        assert!(!left_report.contains(right_head.trim()));
        assert!(!right_report.contains(left_head.trim()));
    }

    #[test]
    fn websocket_route_detection_accepts_a_fragmented_request_line() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accepted socket");
            stream
                .set_read_timeout(Some(StdDuration::from_secs(2)))
                .expect("read timeout");
            peek_websocket_request(&stream)
        });
        let mut client = TcpStream::connect(address).expect("loopback connection");
        client.write_all(b"GET /orna/live/").expect("first fragment");
        std::thread::sleep(StdDuration::from_millis(10));
        client
            .write_all(b"session HTTP/1.1\r\n")
            .expect("request line remainder");
        assert!(server.join().expect("route detection thread"));
    }

    #[test]
    fn default_loopback_listener_serves_the_clone_route() {
        let directory = tempfile::tempdir().expect("temporary clone");
        assert!(
            Command::new("git")
                .args(["init", "--quiet", "--initial-branch", "listener-test"])
                .current_dir(directory.path())
                .status()
                .expect("git")
                .success()
        );
        let identity = RuntimeIdentity {
            database_id: [5; 16],
            repository_id: [6; 16],
        };
        let listener = LiveTransport::bind_default_listener(0).expect("loopback listener");
        let status = listener.status();
        assert!(status.address.ip().is_loopback());
        let root = directory.path().to_path_buf();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.listener().accept().expect("accepted request");
            let mut state = new_serve_state(root, identity, status.address).expect("serve host");
            serve_connection(stream, &mut state).expect("served clone route");
        });

        let mut client = TcpStream::connect(status.address).expect("loopback connection");
        client
            .write_all(
                format!(
                    "GET /api/clone HTTP/1.1\r\nHost: {}\r\n\r\n",
                    status.address
                )
                .as_bytes(),
            )
            .expect("request bytes");
        let mut response = Vec::new();
        client.read_to_end(&mut response).expect("response bytes");
        server.join().expect("server thread");
        let response = String::from_utf8(response).expect("HTTP response");
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("\"HEAD\":null"));
        assert!(response.contains("\"CWD\":{\"branch\":\"listener-test\""));
    }
}
