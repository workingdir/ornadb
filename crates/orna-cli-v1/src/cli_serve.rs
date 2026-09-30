//! The executable edge for `orna serve`.
//!
//! The reference requires a loopback default but does not prescribe URL
//! paths, so this host provides a small stable surface: a rendered project and
//! module browser, per-clone metadata, pure expression queries, Git smart HTTP,
//! and the existing authenticated `orna.present.v1` session/WebSocket routes.

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
    process::{Command, Stdio},
    time::{Duration as StdDuration, SystemTime, UNIX_EPOCH},
};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_GIT_REQUEST_BODY_BYTES: usize = 512 * 1024 * 1024;
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
    query: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

struct Response {
    status: u16,
    content_type: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Response {
    fn new(status: u16, content_type: &str, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            content_type: content_type.into(),
            headers: Vec::new(),
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
    let project = load_project(endpoint)?;
    let catalogue = semantic_catalogue(&project)?;
    let (identity, _) = runtime_identity(&repository)?;
    let listener = LiveTransport::bind_default_listener(port).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "the loopback serving listener could not be bound",
            "choose a free port with `serve --port PORT`, then retry",
        )
    })?;
    let address = listener.status().address;
    let state = new_serve_state(repository.worktree().to_path_buf(), identity, address, catalogue)?;
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
    catalogue: orna_semantic_v1::Catalogue,
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
        catalogue,
        Limits::default(),
    ))
    .with_runtime_identity(identity.database_id, identity.repository_id);
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

// The reference leaves concrete page and Git URLs open, so this host uses
// `/pages/<module path>` for browsable source and mounts standard Git smart
// HTTP at `/git`. The project root is selected once by `orna serve`; requests
// never supply a repository path.
fn host_route(root: &Path, identity: RuntimeIdentity, request: &Request) -> Response {
    if let Some(response) = git_transport_route(root, request) {
        return response;
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => {
            let host = request_header(request, "host")
                .filter(|host| {
                    !host.is_empty()
                        && host.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric()
                                || matches!(byte, b'.' | b'-' | b':' | b'[' | b']')
                        })
                })
                .unwrap_or("127.0.0.1");
            project_home_page(root, identity, host)
        }
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
                        json_string(&format!(
                            "/pages/{}",
                            percent_encode_path(identity.logical_path())
                        )),
                        json_string(identity.logical_path()),
                    ));
                }
                pages.push(']');
                Response::new(200, "application/json", pages.into_bytes())
            }
            Err(_) => unavailable_response(),
        },
        ("GET", path) if path.starts_with("/pages/") => {
            let Ok(requested) = percent_decode(&path[7..]) else {
                return bad_request_response();
            };
            module_source_page(root, &requested)
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

fn project_home_page(root: &Path, identity: RuntimeIdentity, host: &str) -> Response {
    let Ok(project) = load_current_project(root) else {
        return unavailable_response();
    };
    let Ok(report) = clone_report(root, identity) else {
        return unavailable_response();
    };
    let report = String::from_utf8_lossy(&report);
    let clone_url = format!("http://{host}/git");
    let mut modules = String::new();
    for module in project.identities() {
        let logical_path = module.logical_path();
        let href = format!("/pages/{}", percent_encode_path(logical_path));
        modules.push_str(&format!(
            "<li><a href=\"{}\">{}</a></li>",
            html_escape(&href),
            html_escape(logical_path),
        ));
    }
    if modules.is_empty() {
        modules.push_str("<li>No Orna modules are available in this clone.</li>");
    }

    // The reference recommends an ordinary Orna frontend but does not define
    // an installed application contract. This server-rendered project browser
    // therefore renders discovered module and clone data without evaluating
    // source code or requiring an optional UI package.
    let page = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Orna project</title><style>body{{font:16px system-ui,sans-serif;max-width:64rem;margin:2rem auto;padding:0 1rem;color:#17212b}}a{{color:#075985}}pre{{overflow:auto;background:#f1f5f9;padding:1rem;border-radius:.5rem}}input{{min-width:20rem;padding:.5rem}}button{{padding:.5rem}}</style></head><body><header><h1>Orna project</h1><p>Clone: <code>{}</code></p><nav><a href=\"/\">Project</a> · <a href=\"/api/clone\">Clone JSON</a> · <a href=\"/api/pages\">Module index</a></nav><p>Clone this repository with <code>git clone {}</code>.</p></header><main><section><h2>Modules</h2><ul>{}</ul></section><section><h2>Clone HEAD and CWD</h2><pre>{}</pre></section><section><h2>Evaluate an expression</h2><form id=\"query\"><label>Pure Orna expression <input name=\"source\" value=\"1 + 1\"></label> <button>Evaluate</button></form><pre id=\"result\" aria-live=\"polite\"></pre></section></main><script>document.querySelector('#query').addEventListener('submit',async e=>{{e.preventDefault();let r=await fetch('/api/query',{{method:'POST',body:new FormData(e.target).get('source')}});document.querySelector('#result').textContent=await r.text()}})</script></body></html>",
        html_escape(&root.to_string_lossy()),
        html_escape(&clone_url),
        modules,
        html_escape(&report),
    );
    Response::new(200, "text/html; charset=utf-8", page)
}

fn module_source_page(root: &Path, requested: &str) -> Response {
    let Ok(project) = load_current_project(root) else {
        return unavailable_response();
    };
    let Some(index) = project
        .identities()
        .iter()
        .position(|identity| identity.logical_path() == requested)
    else {
        return not_found_response();
    };
    let Some(module) = project.modules().get(index) else {
        return unavailable_response();
    };
    let page = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{} · Orna source</title><style>body{{font:16px system-ui,sans-serif;max-width:72rem;margin:2rem auto;padding:0 1rem;color:#17212b}}a{{color:#075985}}pre{{overflow:auto;background:#0f172a;color:#e2e8f0;padding:1rem;border-radius:.5rem;line-height:1.5}}code{{font-family:ui-monospace,monospace}}</style></head><body><nav><a href=\"/\">← Project</a></nav><main><h1>{}</h1><p>Source from the current clone. Viewing a module does not execute it.</p><pre><code>{}</code></pre></main></body></html>",
        html_escape(requested),
        html_escape(requested),
        html_escape(&module.source),
    );
    Response::new(200, "text/html; charset=utf-8", page)
}

fn html_escape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&#39;"),
            character if character.is_control() && !matches!(character, '\n' | '\r' | '\t') => {
                use std::fmt::Write as _;
                let _ = write!(output, "&#x{:x};", u32::from(character));
            }
            character => output.push(character),
        }
    }
    output
}

fn percent_encode_path(path: &str) -> String {
    let mut encoded = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn percent_decode(path: &str) -> Result<String, ()> {
    let input = path.as_bytes();
    let mut decoded = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] == b'%' {
            let high = input.get(index + 1).and_then(|byte| hex_digit(*byte)).ok_or(())?;
            let low = input.get(index + 2).and_then(|byte| hex_digit(*byte)).ok_or(())?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(input[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| ())
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn git_transport_route(root: &Path, request: &Request) -> Option<Response> {
    let (suffix, service) = match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/git/info/refs") => {
            let service = match request.query.as_str() {
                "service=git-upload-pack" => "git-upload-pack",
                "service=git-receive-pack" => "git-receive-pack",
                _ => return Some(bad_request_response()),
            };
            ("info/refs", service)
        }
        ("POST", "/git/git-upload-pack") => {
            if request_header(request, "content-type")
                != Some("application/x-git-upload-pack-request")
            {
                return Some(bad_request_response());
            }
            ("git-upload-pack", "git-upload-pack")
        }
        ("POST", "/git/git-receive-pack") => {
            if request_header(request, "content-type")
                != Some("application/x-git-receive-pack-request")
            {
                return Some(bad_request_response());
            }
            ("git-receive-pack", "git-receive-pack")
        }
        (_, path) if path.starts_with("/git/") => return Some(not_found_response()),
        _ => return None,
    };
    Some(
        run_git_http_backend(root, request, suffix, service)
            .unwrap_or_else(|_| unavailable_response()),
    )
}

fn request_header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers
        .iter()
        .find(|(header, _)| header.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn run_git_http_backend(
    root: &Path,
    request: &Request,
    suffix: &str,
    service: &str,
) -> io::Result<Response> {
    let repository_name = root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::other("repository path is not representable"))?;
    let project_root = root
        .parent()
        .ok_or_else(|| io::Error::other("repository parent is unavailable"))?;
    let mut command = Command::new("git");
    scrub_host_git_environment(&mut command);
    command
        .arg("http-backend")
        .current_dir(root)
        .env("GIT_PROJECT_ROOT", project_root)
        .env("GIT_HTTP_EXPORT_ALL", "1")
        .env("PATH_INFO", format!("/{repository_name}/{suffix}"))
        .env("SCRIPT_NAME", "/git")
        .env("QUERY_STRING", &request.query)
        .env("REQUEST_METHOD", &request.method)
        .env("SERVER_PROTOCOL", "HTTP/1.1")
        .env("CONTENT_LENGTH", request.body.len().to_string())
        .env(
            "CONTENT_TYPE",
            request_header(request, "content-type").unwrap_or_default(),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(protocol) = request_header(request, "git-protocol") {
        if protocol == "version=2" {
            command.env("GIT_PROTOCOL", protocol);
        }
    }
    // The listener is loopback-only and this profile trusts local OS users.
    // Keep Git's ordinary receive-pack checks (including checked-out-branch
    // protection), while making the standard smart-HTTP push service usable.
    if service == "git-receive-pack" {
        command
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "http.receivepack")
            .env("GIT_CONFIG_VALUE_0", "true");
    }

    let mut child = command.spawn()?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("Git transport input is unavailable"))?;
    let request_body = request.body.clone();
    let input_writer = std::thread::spawn(move || child_stdin.write_all(&request_body));
    let mut cgi_output = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Git transport output is unavailable"))?
        .read_to_end(&mut cgi_output)?;
    input_writer
        .join()
        .map_err(|_| io::Error::other("Git transport input failed"))??;
    if !child.wait()?.success() {
        return Err(io::Error::other("Git HTTP backend failed"));
    }
    parse_cgi_response(&cgi_output)
}

fn scrub_host_git_environment(command: &mut Command) {
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
        "GIT_DISCOVERY_ACROSS_FILESYSTEM",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_NOSYSTEM",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_PREFIX",
        "GIT_NAMESPACE",
        "GIT_REPLACE_REF_BASE",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_GRAFT_FILE",
        "GIT_SHALLOW_FILE",
        "GIT_PROTOCOL",
    ] {
        command.env_remove(variable);
    }
    for (variable, _) in std::env::vars_os() {
        if variable.as_encoded_bytes().starts_with(b"GIT_CONFIG_KEY_")
            || variable
                .as_encoded_bytes()
                .starts_with(b"GIT_CONFIG_VALUE_")
        {
            command.env_remove(variable);
        }
    }
}

fn parse_cgi_response(output: &[u8]) -> io::Result<Response> {
    let header_end = output
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| (position, 4))
        .or_else(|| {
            output
                .windows(2)
                .position(|window| window == b"\n\n")
                .map(|position| (position, 2))
        })
        .ok_or_else(|| io::Error::other("Git HTTP backend returned malformed headers"))?;
    let (header_bytes, separator_len) = header_end;
    let header_text = std::str::from_utf8(&output[..header_bytes])
        .map_err(|_| io::Error::other("Git HTTP backend returned malformed headers"))?;
    let mut status = 200;
    let mut content_type = String::from("application/octet-stream");
    let mut headers = Vec::new();
    for line in header_text.lines() {
        let line = line.trim_end_matches('\r');
        let Some((name, value)) = line.split_once(':') else {
            return Err(io::Error::other("Git HTTP backend returned malformed headers"));
        };
        let value = value.trim();
        if name.eq_ignore_ascii_case("status") {
            status = value
                .split_ascii_whitespace()
                .next()
                .and_then(|code| code.parse().ok())
                .ok_or_else(|| io::Error::other("Git HTTP backend returned malformed status"))?;
        } else if name.eq_ignore_ascii_case("content-type") {
            content_type = value.to_owned();
        } else if ["cache-control", "expires", "pragma"]
            .iter()
            .any(|allowed| name.eq_ignore_ascii_case(allowed))
        {
            headers.push((name.to_owned(), value.to_owned()));
        }
    }
    Ok(Response {
        status,
        content_type,
        headers,
        body: output[header_bytes + separator_len..].to_vec(),
    })
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
    let (path, query) = target
        .split_once('?')
        .map_or((target.as_str(), ""), |(path, query)| (path, query));
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
    let body_limit = if path.starts_with("/git/") {
        MAX_GIT_REQUEST_BODY_BYTES
    } else {
        MAX_REQUEST_BODY_BYTES
    };
    if body_length > body_limit {
        return Err(io::Error::other("request body limit"));
    }
    let mut body = vec![0; body_length];
    reader.read_exact(&mut body)?;
    Ok(Request {
        method,
        path: path.to_owned(),
        query: query.to_owned(),
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
    for (name, value) in &response.headers {
        write!(stream, "{name}: {value}\r\n")?;
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

fn bad_request_response() -> Response {
    Response::new(400, "application/json", br#"{"error":"bad_request"}"#.to_vec())
}

fn unavailable_response() -> Response {
    Response::new(
        503,
        "application/json",
        br#"{"error":"temporarily_unavailable"}"#.to_vec(),
    )
}

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
                query: String::new(),
                headers: Vec::new(),
                body: Vec::new(),
            },
        );
        assert_eq!(pages.status, 200);
        assert!(String::from_utf8(pages.body).unwrap().contains("/pages/main.orna"));
        let home = host_route(
            directory.path(),
            identity,
            &Request {
                method: "GET".into(),
                path: "/".into(),
                query: String::new(),
                headers: Vec::new(),
                body: Vec::new(),
            },
        );
        assert_eq!(home.status, 200);
        let home = String::from_utf8(home.body).expect("project HTML");
        assert!(home.contains("Modules"));
        assert!(home.contains("/pages/main.orna"));
        assert!(home.contains("git clone http://127.0.0.1/git"));
        let source_page = host_route(
            directory.path(),
            identity,
            &Request {
                method: "GET".into(),
                path: "/pages/main.orna".into(),
                query: String::new(),
                headers: Vec::new(),
                body: Vec::new(),
            },
        );
        assert_eq!(source_page.status, 200);
        assert_eq!(source_page.content_type, "text/html; charset=utf-8");
        let source_page = String::from_utf8(source_page.body).expect("source HTML");
        assert!(source_page.contains("1 / 0"));
        assert!(source_page.contains("&lt;script&gt;alert(&#39;source&#39;)&lt;/script&gt;"));
        assert!(!source_page.contains("<script>alert('source')</script>"));
        let query = host_route(
            directory.path(),
            identity,
            &Request {
                method: "POST".into(),
                path: "/api/query".into(),
                query: String::new(),
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
            let mut state = new_serve_state(
                root,
                identity,
                status.address,
                orna_semantic_v1::Catalogue::authoritative_core(),
            )
            .expect("serve host");
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

    #[test]
    fn smart_http_clones_and_pushes_the_selected_clones_head() {
        for (index, branch, dirty) in [
            (0_u8, "http-left", false),
            (1_u8, "http-right", true),
        ] {
            let source = tempfile::tempdir().expect("source clone");
            std::fs::write(source.path().join("main.orna"), SERVE_FIXTURE)
                .expect("vendored project source");
            assert!(
                Command::new("git")
                    .args(["init", "--quiet", "--initial-branch", branch])
                    .current_dir(source.path())
                    .status()
                    .expect("git init")
                    .success()
            );
            assert!(
                Command::new("git")
                    .args([
                        "-c",
                        "user.name=kierandrewett",
                        "-c",
                        "user.email=kieran@drewett.dev",
                        "add",
                        "main.orna",
                    ])
                    .current_dir(source.path())
                    .status()
                    .expect("git add")
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
                        "-m",
                        branch,
                    ])
                    .current_dir(source.path())
                    .status()
                    .expect("git commit")
                    .success()
            );
            if dirty {
                std::fs::write(source.path().join("local-only.txt"), "clone-local CWD")
                    .expect("local CWD change");
            }
            let expected_head = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(source.path())
                .output()
                .expect("source HEAD")
                .stdout;
            let expected_head = String::from_utf8(expected_head)
                .expect("source HEAD UTF-8")
                .trim()
                .to_owned();

            let identity = RuntimeIdentity {
                database_id: [index + 10; 16],
                repository_id: [index + 20; 16],
            };
            let listener = LiveTransport::bind_default_listener(0).expect("loopback listener");
            let address = listener.status().address;
            let root = source.path().to_path_buf();
            let (stop_server, server_stopped) = std::sync::mpsc::channel();
            let server = std::thread::spawn(move || {
                let mut state = new_serve_state(
                    root,
                    identity,
                    address,
                    orna_semantic_v1::Catalogue::authoritative_core(),
                )
                .expect("serve host");
                listener
                    .listener()
                    .set_nonblocking(true)
                    .expect("non-blocking accept");
                loop {
                    match server_stopped.try_recv() {
                        Ok(()) | Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                        Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    }
                    match listener.listener().accept() {
                        Ok((stream, _)) => {
                            serve_connection(stream, &mut state).expect("served Git request");
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(StdDuration::from_millis(2));
                        }
                        Err(error) => panic!("listener accept failed: {error}"),
                    }
                }
            });

            let destination = tempfile::tempdir().expect("client clone parent");
            let clone_path = destination.path().join("checkout");
            let remote = format!("http://{address}/git");
            let cloned = Command::new("git")
                .args(["-c", "http.proxy=", "clone", "--quiet", &remote])
                .arg(&clone_path)
                .output()
                .expect("git clone");
            assert!(
                cloned.status.success(),
                "Git clone failed: {}",
                String::from_utf8_lossy(&cloned.stderr)
            );
            let cloned_head = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&clone_path)
                .output()
                .expect("cloned HEAD")
                .stdout;
            assert_eq!(String::from_utf8(cloned_head).unwrap().trim(), expected_head);
            let pushed = Command::new("git")
                .args([
                    "-c",
                    "http.proxy=",
                    "-C",
                    clone_path.to_str().expect("clone path UTF-8"),
                    "push",
                    "--quiet",
                    &remote,
                    "HEAD:refs/heads/served-client",
                ])
                .output()
                .expect("git push");
            assert!(
                pushed.status.success(),
                "Git push failed: {}",
                String::from_utf8_lossy(&pushed.stderr)
            );
            let published_head = Command::new("git")
                .args(["rev-parse", "refs/heads/served-client"])
                .current_dir(source.path())
                .output()
                .expect("published HEAD")
                .stdout;
            assert_eq!(String::from_utf8(published_head).unwrap().trim(), expected_head);

            let mut client = TcpStream::connect(address).expect("clone report connection");
            client
                .write_all(
                    format!("GET /api/clone HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes(),
                )
                .expect("clone report request");
            let mut response = Vec::new();
            client.read_to_end(&mut response).expect("clone report response");
            let response = String::from_utf8(response).expect("clone report UTF-8");
            assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
            assert!(response.contains(&format!("\"HEAD\":\"{expected_head}\"")));
            assert!(response.contains(&format!("\"branch\":\"{branch}\"")));
            assert!(response.contains(&format!("\"worktree_clean\":{}", !dirty)));
            stop_server.send(()).expect("stop Git server");
            server.join().expect("Git server thread");
        }
    }
}
