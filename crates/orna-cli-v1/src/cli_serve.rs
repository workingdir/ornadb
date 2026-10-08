//! The executable edge for `orna serve`.
//!
//! The host keeps Git-backed pages server-rendered and exposes authenticated
//! runtime evaluation through the live presentation transport.

use super::*;
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use orna_application_v1::{ApplicationLiveAdapter, LIVE_RUN_EVENTS_WATCH_SOURCE};
use orna_live_v1::{
    HttpConnection, LiveHost, LiveSessionAuthority, LiveTransport, SessionMetadata,
    SystemCredentialIssuer, TransportLimits, WireRequest,
};
use orna_protocol_v1::{Envelope, Limits as ProtocolLimits, Message, PresentationContext};
use orna_repository_v1::{
    CommittedBranch, CommittedLogEntry, CommittedTreeEntryKind, GitCommitRef, ManagedPath,
    Repository, RuntimeGeneration,
};
use orna_security_v1::{
    MAX_SESSION_LEASE, Origin, OriginPolicy, SessionBoundary, SessionDeletionAdapter,
};
use orna_serving_v1::Serving;
use orna_syntax_v1::{Declaration, Expr, LiteralKind, Pattern, TypeExpr, parse_module, parse_row};
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration as StdDuration, SystemTime, UNIX_EPOCH},
};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_GIT_REQUEST_BODY_BYTES: usize = 512 * 1024 * 1024;
const SESSION_LEASE_MS: u64 = MAX_SESSION_LEASE;
const SERVE_MODULE_PATH: &str = "main.orna";
const SERVE_ENTRY: &str = "serve";

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
    let state = new_serve_state_with_project(
        repository.worktree().to_path_buf(),
        identity,
        address,
        catalogue,
        Some(project),
    )?;
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
    new_serve_state_with_project(root, identity, listener_address, catalogue, None)
}

fn new_serve_state_with_project(
    root: PathBuf,
    identity: RuntimeIdentity,
    listener_address: SocketAddr,
    catalogue: orna_semantic_v1::Catalogue,
    project: Option<orna_project_v1::LoadedProject>,
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
    let mut application =
        ApplicationLiveAdapter::new(ApplicationAuthority::new(catalogue, Limits::default()))
            .with_runtime_identity(identity.database_id, identity.repository_id)
            .with_module(SERVE_MODULE_PATH, SERVE_ENTRY);
    if let Some(project) = project {
        application = application.with_loaded_project(project);
    }
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

fn serve_listener(listener: &TcpListener, state: ServeState) -> io::Result<()> {
    let root = state.root.clone();
    let identity = state.identity;
    let state = Arc::new(Mutex::new(state));
    for accepted in listener.incoming() {
        let stream = accepted?;
        let connection_state = Arc::clone(&state);
        let connection_root = root.clone();
        std::thread::Builder::new()
            .name("orna-serve-connection".into())
            .spawn(move || {
                let _ =
                    serve_shared_connection(stream, &connection_state, &connection_root, identity);
            })?;
    }
    Ok(())
}

fn serve_connection(mut stream: TcpStream, state: &mut ServeState) -> io::Result<()> {
    stream.set_read_timeout(Some(StdDuration::from_secs(15)))?;
    if peek_websocket_request(&stream) {
        return serve_websocket_connection(stream, state);
    }

    let mut reader = BufReader::new(stream);
    let request = match read_request(&mut reader) {
        Ok(request) => request,
        Err(_) => {
            let mut stream = reader.into_inner();
            write_response(
                &mut stream,
                Response::new(
                    400,
                    "application/json",
                    br#"{"error":"bad_request"}"#.to_vec(),
                ),
            )?;
            return Ok(());
        }
    };
    stream = reader.into_inner();
    if request.path.starts_with("/orna/session") {
        return serve_session_http_request(stream, request, state);
    }
    let response = host_route(&state.root, state.identity, &request);
    write_response(&mut stream, response)
}

fn serve_shared_connection(
    mut stream: TcpStream,
    state: &Mutex<ServeState>,
    root: &Path,
    identity: RuntimeIdentity,
) -> io::Result<()> {
    stream.set_read_timeout(Some(StdDuration::from_secs(15)))?;
    if peek_websocket_request(&stream) {
        stream.set_read_timeout(None)?;
        let mut state = state
            .lock()
            .map_err(|_| io::Error::other("live session state is unavailable"))?;
        return serve_websocket_connection(stream, &mut state);
    }

    let mut reader = BufReader::new(stream);
    let request = match read_request(&mut reader) {
        Ok(request) => request,
        Err(_) => {
            let mut stream = reader.into_inner();
            write_response(
                &mut stream,
                Response::new(
                    400,
                    "application/json",
                    br#"{"error":"bad_request"}"#.to_vec(),
                ),
            )?;
            return Ok(());
        }
    };
    stream = reader.into_inner();
    if request.path.starts_with("/orna/session") {
        let mut state = state
            .lock()
            .map_err(|_| io::Error::other("live session state is unavailable"))?;
        return serve_session_http_request(stream, request, &mut state);
    }
    write_response(&mut stream, host_route(root, identity, &request))
}

fn serve_websocket_connection(stream: TcpStream, state: &mut ServeState) -> io::Result<()> {
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
    Ok(())
}

fn serve_session_http_request(
    mut stream: TcpStream,
    request: Request,
    state: &mut ServeState,
) -> io::Result<()> {
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
    stream.write_all(&bytes)
}

// The project root is selected once by `orna serve`; requests never supply a
// repository path. Browsing is based on immutable Git commits and trees.
fn host_route(root: &Path, identity: RuntimeIdentity, request: &Request) -> Response {
    if let Some(response) = git_transport_route(root, request) {
        return response;
    }
    if let Some(response) = git_listing_route(root, identity, request) {
        return response;
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/api/examples") => playground_examples(root),
        ("GET", "/api/playground/revision") => playground_revision(root),
        ("GET", "/api/playground/catalogue") => playground_catalogue_json(root),
        ("GET", "/api/clone") => match clone_report(root, identity) {
            Ok(report) => Response::new(200, "application/json", report),
            Err(_) => unavailable_response(),
        },
        _ => not_found_response(),
    }
}

const MAX_LISTING_COMMITS: usize = 100;
const MAX_LISTING_TREE_ENTRIES: usize = 10_000;
const MAX_LISTING_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_DEVTOOLS_SOURCE_FILES: usize = 2_048;
const MAX_DEVTOOLS_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const LISTING_STYLE: &str = "<style>:root{--page-width:72ch;--text:#202122;--background:#fff;--link:#0645ad;--visited:#0b0080;--rule:#a2a9b1;--body-font:Georgia,'Times New Roman',serif;--code-font:ui-monospace,monospace}body{max-width:var(--page-width);margin:1.5rem auto;padding:0 1rem;color:var(--text);background:var(--background);font:1rem/1.5 var(--body-font)}a{color:var(--link)}a:visited{color:var(--visited)}pre,textarea{overflow:auto;white-space:pre-wrap;overflow-wrap:anywhere;font-family:var(--code-font)}textarea{box-sizing:border-box;width:100%}button{font:inherit}table{border-collapse:collapse}th,td{border:1px solid var(--rule);padding:.2rem .45rem;text-align:left;vertical-align:top}</style>";
/// Renderer-neutral data used by the simple Inspect-compatible HTML fallback.
enum InspectionNode {
    Text(String),
    Link { label: String, href: String },
    Record(Vec<(String, Self)>),
    List(Vec<Self>),
    OrderedList(Vec<Self>),
    Code(String),
}

struct Breadcrumb {
    label: String,
    href: String,
}

fn git_listing_route(
    root: &Path,
    identity: RuntimeIdentity,
    request: &Request,
) -> Option<Response> {
    if request.method != "GET" {
        return None;
    }
    match request.path.as_str() {
        "/" => Some(commit_log_page(root, identity)),
        "/devtools/tables" => Some(devtools_tables_page(root)),
        "/playground/theme.css" => Some(playground_style(
            root,
            Path::new("playground/web-ui/src/theme.css"),
            &request.query,
        )),
        "/playground/layout.css" => Some(playground_style(
            root,
            Path::new("playground/web-ui/src/layout.css"),
            &request.query,
        )),
        "/playground/catalogue" | "/playground/catalogue/" => {
            Some(playground_catalogue_page(root, identity, &request.query))
        }
        "/playground" => Some(playground_asset(root, identity, &request.path)),
        path if path.starts_with("/playground/") => {
            Some(playground_asset(root, identity, &request.path))
        }
        path if path.starts_with("/tree/") => Some(tree_page(root, &path[6..])),
        path if path.starts_with("/blob/") => Some(blob_page(root, &path[6..])),
        _ => None,
    }
}

fn commit_log_page(root: &Path, identity: RuntimeIdentity) -> Response {
    let Ok(repository) = Repository::discover(root) else {
        return unavailable_response();
    };
    let Ok(entries) = repository.committed_history(MAX_LISTING_COMMITS) else {
        return unavailable_response();
    };
    let rows = entries.iter().map(commit_log_node).collect::<Vec<_>>();
    let commits = if rows.is_empty() {
        InspectionNode::Text("No commits in this repository.".into())
    } else {
        InspectionNode::OrderedList(rows)
    };
    let Ok(branches) = repository.local_branches(MAX_LISTING_BRANCHES) else {
        return unavailable_response();
    };
    let branch_rows = branches.iter().map(branch_node).collect::<Vec<_>>();
    let branches = if branch_rows.is_empty() {
        InspectionNode::Text("No local branches in this repository.".into())
    } else {
        InspectionNode::List(branch_rows)
    };
    let content = InspectionNode::Record(vec![
        (
            "Database tables".into(),
            InspectionNode::Link {
                label: "Browse database tables".into(),
                href: "/devtools/tables".into(),
            },
        ),
        (
            "Playground".into(),
            InspectionNode::Link {
                label: "Open the Orna playground".into(),
                href: "/playground/".into(),
            },
        ),
        ("Recent commits".into(), commits),
        ("Branches".into(), branches),
    ]);
    render_home_document(identity, &content)
}

struct DevtoolsTable {
    name: String,
    source_path: String,
    keys: Vec<String>,
    fields: Vec<String>,
}

fn devtools_tables_page(root: &Path) -> Response {
    let Ok(repository) = Repository::discover(root) else {
        return unavailable_response();
    };
    let Ok(Some(commit)) = repository.head() else {
        return unavailable_response();
    };
    let Ok(entries) = repository.list_committed_tree(&commit, MAX_LISTING_TREE_ENTRIES) else {
        return unavailable_response();
    };

    let mut source_files = entries
        .into_iter()
        .filter(|entry| {
            matches!(entry.kind(), CommittedTreeEntryKind::File { .. })
                && entry
                    .path()
                    .as_path()
                    .extension()
                    .and_then(std::ffi::OsStr::to_str)
                    == Some("orna")
                && !is_committed_row_path(entry.path().as_path())
        })
        .collect::<Vec<_>>();
    source_files.sort_by(|left, right| left.path().as_path().cmp(right.path().as_path()));

    let mut tables = Vec::new();
    let mut source_bytes = 0usize;
    let mut source_files_read = 0usize;
    for entry in source_files {
        if source_files_read >= MAX_DEVTOOLS_SOURCE_FILES {
            return unavailable_response();
        }
        source_files_read += 1;
        let path = entry.path().as_path();
        let Ok(source) = repository.read_committed_file(&commit, path, MAX_LISTING_FILE_BYTES)
        else {
            return unavailable_response();
        };
        let Some(next_source_bytes) = source_bytes.checked_add(source.len()) else {
            return unavailable_response();
        };
        if next_source_bytes > MAX_DEVTOOLS_SOURCE_BYTES {
            return unavailable_response();
        }
        source_bytes = next_source_bytes;
        let Ok(source) = String::from_utf8(source) else {
            continue;
        };
        let parsed = parse_module(&source);
        if !parsed.is_ok() {
            continue;
        }
        for item in &parsed.value.items {
            let Declaration::Table {
                name,
                keys,
                members,
                ..
            } = &item.declaration
            else {
                continue;
            };
            let key_names = keys
                .iter()
                .filter_map(|key| match &key.pattern {
                    Pattern::Name(name, _) => Some(name.clone()),
                    _ => None,
                })
                .collect();
            let field_names = members
                .iter()
                .filter_map(|member| match member {
                    orna_syntax_v1::TableMember::Field { name, .. } => Some(name.clone()),
                    _ => None,
                })
                .collect();
            tables.push(DevtoolsTable {
                name: name.clone(),
                source_path: path.to_string_lossy().into_owned(),
                keys: key_names,
                fields: field_names,
            });
        }
    }

    tables.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.source_path.cmp(&right.source_path))
    });
    let table_nodes = tables
        .into_iter()
        .map(|table| {
            let source_href = format!(
                "/blob/{}/{}",
                commit.as_str(),
                percent_encode_path(&table.source_path)
            );
            InspectionNode::Record(vec![
                ("Table".into(), InspectionNode::Text(table.name)),
                (
                    "Source".into(),
                    InspectionNode::Link {
                        label: table.source_path,
                        href: source_href,
                    },
                ),
                (
                    "Key fields".into(),
                    inspection_names(table.keys, "No declared key fields."),
                ),
                (
                    "Fields".into(),
                    inspection_names(table.fields, "No declared fields."),
                ),
            ])
        })
        .collect::<Vec<_>>();
    let content = InspectionNode::Record(vec![
        (
            "Refresh".into(),
            InspectionNode::Link {
                label: "Refresh the committed table list".into(),
                href: "/devtools/tables".into(),
            },
        ),
        (
            "Committed revision".into(),
            InspectionNode::Text(commit.as_str().to_owned()),
        ),
        (
            "Tables".into(),
            if table_nodes.is_empty() {
                InspectionNode::Text("No table declarations in committed source.".into())
            } else {
                InspectionNode::List(table_nodes)
            },
        ),
    ]);
    let mut response = render_inspection_document("Database tables", &[], &content);
    response
        .headers
        .push(("Cache-Control".into(), "no-store".into()));
    response
}

fn inspection_names(names: Vec<String>, empty_message: &str) -> InspectionNode {
    if names.is_empty() {
        InspectionNode::Text(empty_message.into())
    } else {
        InspectionNode::List(names.into_iter().map(InspectionNode::Text).collect())
    }
}

fn is_committed_row_path(path: &Path) -> bool {
    path.parent().is_some_and(|parent| {
        parent.components().any(|component| {
            component
                .as_os_str()
                .to_string_lossy()
                .chars()
                .next()
                .is_some_and(char::is_uppercase)
        })
    })
}

const MAX_PLAYGROUND_CATALOGUE_ENTRIES: usize = 4096;
const PLAYGROUND_CATALOGUE_PAGE_SIZE: usize = 25;
const MAX_PLAYGROUND_ASSET_BYTES: usize = 8 * 1024 * 1024;
const MAX_PLAYGROUND_ASSET_ROW_BYTES: usize = 16 * 1024 * 1024;
const MAX_PLAYGROUND_STYLE_BYTES: usize = 256 * 1024;
const MAX_PLAYGROUND_EXAMPLE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PLAYGROUND_SAMPLE_BYTES: usize = 64 * 1024;
const MAX_PLAYGROUND_FILE_EXAMPLES: usize = 100;
const MAX_PLAYGROUND_SAMPLE_ROWS: usize = 100;
const MAX_PLAYGROUND_EXAMPLES_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
fn playground_asset(root: &Path, identity: RuntimeIdentity, request_path: &str) -> Response {
    let Some(route_path) = normalize_playground_route_path(request_path) else {
        return bad_request_response();
    };
    let (media_type, content, kind) = match read_playground_asset(root, &route_path) {
        Ok(Some(asset)) => asset,
        Ok(None) => return not_found_response(),
        Err(()) => return unavailable_response(),
    };
    if kind != PlaygroundEntryKind::Asset {
        let Ok(mut page) = String::from_utf8(content) else {
            return unavailable_response();
        };
        if kind == PlaygroundEntryKind::Embed && !hide_embedded_page_header(&mut page) {
            return unavailable_response();
        }
        let database = format_uuid(identity.database_id);
        let bridge = format!(
            "<section id=\"live-bridge\" data-database=\"{}\" hidden></section><section aria-labelledby=\"live-title\"><h2 id=\"live-title\">Live presentation</h2><p id=\"live-status\" role=\"status\" aria-live=\"polite\">Connecting to the database…</p><div id=\"live-presentation\"></div></section><script type=\"application/json\" id=\"run-events-source\">{}</script><script type=\"module\" src=\"/playground/assets/serve-playground.mjs\"></script>",
            html_escape(&database),
            json_string(LIVE_RUN_EVENTS_WATCH_SOURCE),
        );
        if let Some(body) = find_html_close_tag(&page, b"</body>") {
            page.insert_str(body, &bridge);
        } else {
            return bad_request_response();
        }
        let mut response = Response::new(200, "text/html; charset=utf-8", page.into_bytes());
        response
            .headers
            .push(("X-Content-Type-Options".into(), "nosniff".into()));
        if kind == PlaygroundEntryKind::Embed {
            response
                .headers
                .push(("Content-Security-Policy".into(), "frame-ancestors *".into()));
        }
        return response;
    }
    let mut response = Response::new(200, media_type, content);
    response
        .headers
        .push(("X-Content-Type-Options".into(), "nosniff".into()));
    response
}

fn playground_revision(root: &Path) -> Response {
    let Ok(repository) = Repository::discover(root) else {
        return unavailable_response();
    };
    let Ok(Some(commit)) = repository.head() else {
        return unavailable_response();
    };
    let mut response = Response::new(
        200,
        "application/json",
        format!("{{\"revision\":{}}}", json_string(commit.as_str())).into_bytes(),
    );
    response
        .headers
        .push(("Cache-Control".into(), "no-store".into()));
    response
}

fn playground_style(root: &Path, resource: &Path, query: &str) -> Response {
    let Ok(repository) = Repository::discover(root) else {
        return unavailable_response();
    };
    let revision = match playground_style_revision(query) {
        Ok(revision) => revision,
        Err(()) => return bad_request_response(),
    };
    let commit = match revision {
        Some(revision) => match resolve_listing_commit(&repository, revision) {
            Some(commit) => commit,
            None => return not_found_response(),
        },
        None => match repository.head() {
            Ok(Some(commit)) => commit,
            _ => return unavailable_response(),
        },
    };
    let Ok(css) = repository.read_committed_file(&commit, resource, MAX_PLAYGROUND_STYLE_BYTES)
    else {
        return not_found_response();
    };
    let Ok(css) = String::from_utf8(css) else {
        return unavailable_response();
    };
    let mut response = Response::new(200, "text/css; charset=utf-8", css.into_bytes());
    response
        .headers
        .push(("Cache-Control".into(), "no-cache".into()));
    response
        .headers
        .push(("X-Content-Type-Options".into(), "nosniff".into()));
    response
}

fn playground_style_revision(query: &str) -> Result<Option<&str>, ()> {
    if query.is_empty() {
        return Ok(None);
    }
    let mut parameters = query.split('&');
    let Some((name, revision)) = parameters
        .next()
        .and_then(|parameter| parameter.split_once('='))
    else {
        return Err(());
    };
    if name != "revision" || revision.is_empty() || parameters.next().is_some() {
        return Err(());
    }
    Ok(Some(revision))
}

fn normalize_playground_route_path(request_path: &str) -> Option<String> {
    let relative = request_path.strip_prefix("/playground")?;
    let relative = relative.strip_prefix('/').unwrap_or(relative);
    let decoded = percent_decode(relative).ok()?;
    if decoded.contains('\\')
        || decoded.contains('\0')
        || decoded.starts_with('/')
        || decoded.contains("//")
        || decoded
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return None;
    }
    let relative = match decoded.as_str() {
        "" => return Some("/playground/".into()),
        "embed/" => "embed",
        _ => decoded.as_str(),
    };
    Some(format!("/playground/{relative}"))
}

fn encoded_playground_row_id(prefix: &str, value: &str) -> String {
    format!(
        "{prefix}{}",
        value
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

/// Lists the playground routes committed as DB-resident `playground/Route` rows.
/// Each row is decoded with the same validation the route serving path uses, and
/// rows that fail decoding are left out rather than listed.
fn playground_catalogue_routes(root: &Path) -> Result<Vec<String>, ()> {
    let repository = Repository::discover(root).map_err(|_| ())?;
    let Some(commit) = repository.head().map_err(|_| ())? else {
        return Ok(Vec::new());
    };
    let tree = repository
        .list_committed_tree(&commit, MAX_PLAYGROUND_CATALOGUE_ENTRIES)
        .map_err(|_| ())?;
    let mut routes = Vec::new();
    for entry in tree {
        let path = entry.path().as_path();
        if path.parent() != Some(Path::new("playground/Route"))
            || path.extension().is_none_or(|ext| ext != "orna")
        {
            continue;
        }
        let Some(file_name) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if !matches!(entry.kind(), CommittedTreeEntryKind::File { .. }) {
            continue;
        }
        let Ok(source) = repository.read_committed_file(
            &commit,
            path,
            MAX_PLAYGROUND_ASSET_ROW_BYTES,
        ) else {
            continue;
        };
        let Ok(source) = String::from_utf8(source) else {
            continue;
        };
        if let Some((route_path, _)) = decode_playground_route(&source, file_name) {
            routes.push(route_path);
        }
    }
    routes.sort();
    routes.dedup();
    Ok(routes)
}

/// JSON rows for the playground catalogue: the committed route paths, read from
/// the DB-resident `playground/Route` rows and sorted.
fn playground_catalogue_json(root: &Path) -> Response {
    let Ok(routes) = playground_catalogue_routes(root) else {
        return unavailable_response();
    };
    let rows = routes
        .iter()
        .map(|route| json_string(route))
        .collect::<Vec<_>>()
        .join(",");
    let mut response = Response::new(
        200,
        "application/json",
        format!("{{\"routes\":[{rows}]}}").into_bytes(),
    );
    response
        .headers
        .push(("Cache-Control".into(), "no-store".into()));
    response
}

/// Parses the optional `page=N` parameter for the catalogue page. Pages are
/// 1-based; an absent parameter is the first page.
fn playground_catalogue_page_number(query: &str) -> Result<usize, ()> {
    if query.is_empty() {
        return Ok(1);
    }
    let Some(number) = query.strip_prefix("page=") else {
        return Err(());
    };
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    match number.parse::<usize>() {
        Ok(page) if page > 0 => Ok(page),
        _ => Err(()),
    }
}

fn playground_catalogue_page(root: &Path, identity: RuntimeIdentity, query: &str) -> Response {
    let Ok(page) = playground_catalogue_page_number(query) else {
        return bad_request_response();
    };
    let Ok(routes) = playground_catalogue_routes(root) else {
        return unavailable_response();
    };
    let start = (page - 1).saturating_mul(PLAYGROUND_CATALOGUE_PAGE_SIZE);
    let rows = routes
        .iter()
        .skip(start)
        .take(PLAYGROUND_CATALOGUE_PAGE_SIZE)
        .map(|route| InspectionNode::Link {
            label: route.clone(),
            href: route.clone(),
        })
        .collect::<Vec<_>>();
    let has_next = routes.len() > start.saturating_add(PLAYGROUND_CATALOGUE_PAGE_SIZE);
    let content = if rows.is_empty() {
        InspectionNode::Text("No playground catalogue entries are committed.".into())
    } else {
        InspectionNode::List(rows)
    };
    let mut sections = vec![("Catalogue".into(), content)];
    if has_next {
        sections.push((
            "More".into(),
            InspectionNode::Link {
                label: "Next page".into(),
                href: format!("/playground/catalogue?page={}", page + 1),
            },
        ));
    }
    render_home_document(identity, &InspectionNode::Record(sections))
}

fn playground_route_record_path(route_path: &str) -> Option<(ManagedPath, String)> {
    if !route_path.starts_with("/playground/") {
        return None;
    }
    let id = encoded_playground_row_id("route-", route_path);
    let record_path =
        ManagedPath::new(Path::new("playground/Route").join(format!("{id}.orna"))).ok()?;
    Some((record_path, id))
}

fn playground_entry_record_path(entry_id: &str) -> Option<ManagedPath> {
    if entry_id.is_empty()
        || entry_id.len() > 128
        || !entry_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return None;
    }
    ManagedPath::new(Path::new("playground/Entry").join(format!("{entry_id}.orna"))).ok()
}

fn playground_asset_record_path(asset_path: &str) -> Option<(ManagedPath, String)> {
    let asset_path = ManagedPath::new(Path::new(asset_path)).ok()?;
    let normalized = asset_path.as_path().to_str()?;
    if normalized != "index.html" && !normalized.starts_with("assets/") {
        return None;
    }
    let media_type = playground_content_type(asset_path.as_path());
    if media_type == "application/octet-stream" {
        return None;
    }
    let id = format!(
        "asset-{}",
        normalized
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let record_path =
        ManagedPath::new(Path::new("playground/Asset").join(format!("{id}.orna"))).ok()?;
    Some((record_path, id))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlaygroundEntryKind {
    Page,
    Embed,
    Asset,
}

fn read_playground_asset(
    root: &Path,
    requested_route_path: &str,
) -> Result<Option<(&'static str, Vec<u8>, PlaygroundEntryKind)>, ()> {
    let repository = Repository::discover(root).map_err(|_| ())?;
    let Some(commit) = repository.head().map_err(|_| ())? else {
        return Err(());
    };
    let schema = repository
        .read_committed_file(
            &commit,
            Path::new("playground.orna"),
            MAX_PLAYGROUND_SAMPLE_BYTES,
        )
        .map_err(|_| ())?;
    let schema = String::from_utf8(schema).map_err(|_| ())?;
    if !has_playground_asset_table(&schema)
        || !has_playground_route_table(&schema)
        || !has_playground_entry_table(&schema)
    {
        return Err(());
    }
    let (route_record, route_id) = playground_route_record_path(requested_route_path).ok_or(())?;
    let Ok(source) = repository.read_committed_file(
        &commit,
        route_record.as_path(),
        MAX_PLAYGROUND_ASSET_ROW_BYTES,
    ) else {
        return Ok(None);
    };
    let source = String::from_utf8(source).map_err(|_| ())?;
    let Some((route_path, entry_id)) = decode_playground_route(&source, &route_id) else {
        return Err(());
    };
    if route_path != requested_route_path {
        return Err(());
    }
    let entry_record = playground_entry_record_path(&entry_id).ok_or(())?;
    let entry_source = repository
        .read_committed_file(
            &commit,
            entry_record.as_path(),
            MAX_PLAYGROUND_ASSET_ROW_BYTES,
        )
        .map_err(|_| ())?;
    let entry_source = String::from_utf8(entry_source).map_err(|_| ())?;
    let Some((asset_path, kind)) = decode_playground_entry(&entry_source, &entry_id) else {
        return Err(());
    };
    if (kind == PlaygroundEntryKind::Asset && !asset_path.starts_with("assets/"))
        || (kind != PlaygroundEntryKind::Asset && asset_path != "index.html")
    {
        return Err(());
    }
    let (asset_record, asset_id) = playground_asset_record_path(&asset_path).ok_or(())?;
    let asset_source = repository
        .read_committed_file(
            &commit,
            asset_record.as_path(),
            MAX_PLAYGROUND_ASSET_ROW_BYTES,
        )
        .map_err(|_| ())?;
    let asset_source = String::from_utf8(asset_source).map_err(|_| ())?;
    let Some((path, media_type, content)) = decode_playground_asset(&asset_source, &asset_id)
    else {
        return Err(());
    };
    let expected_media_type = playground_content_type(Path::new(&path));
    if path != asset_path
        || media_type != expected_media_type
        || media_type == "application/octet-stream"
    {
        return Err(());
    }
    let content = if media_type == "application/wasm" {
        BASE64.decode(content.as_bytes()).map_err(|_| ())?
    } else {
        content.into_bytes()
    };
    if content.len() > MAX_PLAYGROUND_ASSET_BYTES {
        return Err(());
    }
    Ok(Some((expected_media_type, content, kind)))
}

fn hide_embedded_page_header(page: &mut String) -> bool {
    const MARKER: &str = "data-page-header";
    let Some(marker) = page.find(MARKER) else {
        return false;
    };
    if page[marker + MARKER.len()..].contains(MARKER) {
        return false;
    }
    let Some(header_start) = page[..marker].rfind("<header") else {
        return false;
    };
    let name_end = header_start + "<header".len();
    if !page[name_end..].starts_with(char::is_whitespace)
        || page[name_end..marker].contains('>')
        || !page[marker + MARKER.len()..]
            .chars()
            .next()
            .is_some_and(|character| character.is_whitespace() || matches!(character, '>' | '/'))
    {
        return false;
    }
    page.insert_str(marker + MARKER.len(), " hidden");
    true
}

fn find_html_close_tag(html: &str, tag: &[u8]) -> Option<usize> {
    html.as_bytes()
        .windows(tag.len())
        .rposition(|window| window.eq_ignore_ascii_case(tag))
}

fn playground_content_type(path: &Path) -> &'static str {
    match path.extension().and_then(std::ffi::OsStr::to_str) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("eot") => "application/vnd.ms-fontobject",
        _ => "application/octet-stream",
    }
}

fn playground_examples(root: &Path) -> Response {
    let Ok(repository) = Repository::discover(root) else {
        return unavailable_response();
    };
    let Ok(Some(commit)) = repository.head() else {
        return unavailable_response();
    };
    let sample_table_available = repository
        .read_committed_file(
            &commit,
            Path::new("playground.orna"),
            MAX_PLAYGROUND_SAMPLE_BYTES,
        )
        .ok()
        .and_then(|schema| String::from_utf8(schema).ok())
        .is_some_and(|schema| has_playground_sample_table(&schema));
    let Ok(entries) = repository.list_committed_tree(&commit, MAX_LISTING_TREE_ENTRIES) else {
        return unavailable_response();
    };
    let mut entries = entries;
    entries.sort_by(|left, right| left.path().as_path().cmp(right.path().as_path()));
    let mut examples = Vec::new();
    let mut examples_bytes = 0usize;
    let mut scanned_file_examples = 0usize;
    let mut scanned_sample_rows = 0usize;
    for entry in entries {
        let path = entry.path().as_path();
        if !matches!(entry.kind(), CommittedTreeEntryKind::File { .. })
            || path.extension().and_then(std::ffi::OsStr::to_str) != Some("orna")
        {
            continue;
        }
        let sample_row =
            sample_table_available && path.parent() == Some(Path::new("playground/Sample"));
        let file_example = path.starts_with("playground/examples");
        if sample_row {
            if scanned_sample_rows >= MAX_PLAYGROUND_SAMPLE_ROWS {
                continue;
            }
            scanned_sample_rows += 1;
        } else if file_example {
            if scanned_file_examples >= MAX_PLAYGROUND_FILE_EXAMPLES {
                continue;
            }
            scanned_file_examples += 1;
        } else {
            continue;
        }
        let read_limit = if sample_row {
            MAX_PLAYGROUND_SAMPLE_BYTES
        } else {
            MAX_PLAYGROUND_EXAMPLE_BYTES
        };
        let Ok(source) = repository.read_committed_file(&commit, path, read_limit) else {
            continue;
        };
        let Ok(source) = String::from_utf8(source) else {
            continue;
        };
        let (name, source) = if sample_row {
            let Some(id) = path.file_stem().and_then(std::ffi::OsStr::to_str) else {
                continue;
            };
            let Some((name, source)) = decode_playground_sample(&source, id) else {
                continue;
            };
            (name, source)
        } else {
            let name = path
                .file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("example")
                .to_owned();
            (name, source)
        };
        let example = serde_json::json!({
            "name": name,
            "path": path.to_string_lossy(),
            "source": source,
        });
        let Ok(example_bytes) = serde_json::to_vec(&example) else {
            continue;
        };
        let Some(next_bytes) = examples_bytes.checked_add(example_bytes.len()) else {
            break;
        };
        if next_bytes > MAX_PLAYGROUND_EXAMPLES_RESPONSE_BYTES.saturating_sub(128) {
            continue;
        }
        examples_bytes = next_bytes;
        examples.push(example);
    }
    examples.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    match serde_json::to_vec(&serde_json::json!({
        "revision": commit.as_str(),
        "examples": examples,
    })) {
        Ok(body) => Response::new(200, "application/json", body),
        Err(_) => unavailable_response(),
    }
}

fn has_playground_asset_table(source: &str) -> bool {
    has_playground_record_table(source, "Asset", &["path", "media_type", "content"])
}

fn has_playground_route_table(source: &str) -> bool {
    has_playground_record_table(source, "Route", &["path", "entry"])
}

fn has_playground_entry_table(source: &str) -> bool {
    has_playground_record_table(source, "Entry", &["asset_path", "kind"])
}

fn has_playground_record_table(source: &str, table_name: &str, required_fields: &[&str]) -> bool {
    let parsed = parse_module(source);
    if !parsed.is_ok() {
        return false;
    }
    let mut tables = parsed.value.items.iter().filter_map(|item| {
        let Declaration::Table {
            name,
            keys,
            members,
        } = &item.declaration
        else {
            return None;
        };
        (name == table_name).then_some((keys, members))
    });
    let Some((keys, members)) = tables.next() else {
        return false;
    };
    if tables.next().is_some()
        || keys.len() != 1
        || !matches!(
            &keys[0],
            orna_syntax_v1::Parameter {
                pattern: Pattern::Name(name, _),
                annotation: Some(ty),
                ..
            } if name == "id" && is_string_type(ty)
        )
    {
        return false;
    }
    required_fields.iter().all(|required| {
        let mut fields = members.iter().filter_map(|member| match member {
            orna_syntax_v1::TableMember::Field { name, ty, .. } if name == required => Some(ty),
            _ => None,
        });
        fields.next().is_some_and(is_string_type) && fields.next().is_none()
    })
}

fn decode_playground_asset(source: &str, expected_id: &str) -> Option<(String, String, String)> {
    let parsed = parse_row(source);
    if !parsed.is_ok() {
        return None;
    }
    let Expr::Record { fields, .. } = parsed.value else {
        return None;
    };
    let id = literal_string(unique_record_field(&fields, "id")?)?;
    if id != expected_id {
        return None;
    }
    let path = literal_string(unique_record_field(&fields, "path")?)?;
    let media_type = literal_string(unique_record_field(&fields, "media_type")?)?;
    let content = literal_string(unique_record_field(&fields, "content")?)?;
    Some((path, media_type, content))
}

fn decode_playground_route(source: &str, expected_id: &str) -> Option<(String, String)> {
    let parsed = parse_row(source);
    if !parsed.is_ok() {
        return None;
    }
    let Expr::Record { fields, .. } = parsed.value else {
        return None;
    };
    let id = literal_string(unique_record_field(&fields, "id")?)?;
    if id != expected_id {
        return None;
    }
    let path = literal_string(unique_record_field(&fields, "path")?)?;
    let entry = literal_string(unique_record_field(&fields, "entry")?)?;
    Some((path, entry))
}

fn decode_playground_entry(
    source: &str,
    expected_id: &str,
) -> Option<(String, PlaygroundEntryKind)> {
    let parsed = parse_row(source);
    if !parsed.is_ok() {
        return None;
    }
    let Expr::Record { fields, .. } = parsed.value else {
        return None;
    };
    let id = literal_string(unique_record_field(&fields, "id")?)?;
    if id != expected_id {
        return None;
    }
    let asset_path = literal_string(unique_record_field(&fields, "asset_path")?)?;
    let kind = match literal_string(unique_record_field(&fields, "kind")?)?.as_str() {
        "page" => PlaygroundEntryKind::Page,
        "embed" => PlaygroundEntryKind::Embed,
        "asset" => PlaygroundEntryKind::Asset,
        _ => return None,
    };
    Some((asset_path, kind))
}

fn has_playground_sample_table(source: &str) -> bool {
    let parsed = parse_module(source);
    if !parsed.is_ok() {
        return false;
    }
    let mut tables = parsed.value.items.iter().filter_map(|item| {
        let Declaration::Table {
            name,
            keys,
            members,
        } = &item.declaration
        else {
            return None;
        };
        (name == "Sample").then_some((keys, members))
    });
    let Some((keys, members)) = tables.next() else {
        return false;
    };
    if tables.next().is_some()
        || keys.len() != 1
        || !matches!(
            &keys[0],
            orna_syntax_v1::Parameter {
                pattern: Pattern::Name(name, _),
                annotation: Some(ty),
                ..
            } if name == "id" && is_string_type(ty)
        )
    {
        return false;
    }
    ["name", "source"].iter().all(|required| {
        let mut fields = members.iter().filter_map(|member| match member {
            orna_syntax_v1::TableMember::Field { name, ty, .. } if name == required => Some(ty),
            _ => None,
        });
        fields.next().is_some_and(is_string_type) && fields.next().is_none()
    })
}

fn is_string_type(ty: &TypeExpr) -> bool {
    matches!(ty, TypeExpr::Name { path, arguments, .. } if path.len() == 1 && path[0] == "Str" && arguments.is_empty())
}

fn decode_playground_sample(source: &str, expected_id: &str) -> Option<(String, String)> {
    let parsed = parse_row(source);
    if !parsed.is_ok() {
        return None;
    }
    let Expr::Record { fields, .. } = parsed.value else {
        return None;
    };
    let id = literal_string(unique_record_field(&fields, "id")?)?;
    let name = literal_string(unique_record_field(&fields, "name")?)?;
    let source = literal_string(unique_record_field(&fields, "source")?)?;
    if id != expected_id || name.trim().is_empty() || source.trim().is_empty() {
        return None;
    }
    Some((name, source))
}

fn unique_record_field<'a>(
    fields: &'a [orna_syntax_v1::RecordField],
    name: &str,
) -> Option<&'a orna_syntax_v1::RecordField> {
    let mut matches = fields.iter().filter(|field| field.name == name);
    let field = matches.next()?;
    matches.next().is_none().then_some(field)
}

fn literal_string(field: &orna_syntax_v1::RecordField) -> Option<String> {
    let Expr::Literal {
        text,
        kind: LiteralKind::String,
        ..
    } = &field.value
    else {
        return None;
    };
    decode_orna_string(text)
}

fn decode_orna_string(text: &str) -> Option<String> {
    let body = text.strip_prefix('"')?.strip_suffix('"')?;
    let mut decoded = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        match chars.next()? {
            '"' => decoded.push('"'),
            '\\' => decoded.push('\\'),
            'n' => decoded.push('\n'),
            'r' => decoded.push('\r'),
            't' => decoded.push('\t'),
            '0' => decoded.push('\0'),
            'u' => {
                if chars.next()? != '{' {
                    return None;
                }
                let digits = chars
                    .by_ref()
                    .take_while(|character| *character != '}')
                    .collect::<String>();
                let scalar = u32::from_str_radix(&digits, 16).ok()?;
                decoded.push(char::from_u32(scalar)?);
            }
            _ => return None,
        }
    }
    Some(decoded)
}

const MAX_LISTING_BRANCHES: usize = 100;

fn commit_log_node(entry: &CommittedLogEntry) -> InspectionNode {
    let commit = entry.commit().as_str();
    let href = format!("/tree/{commit}/");
    let subject = if entry.subject().is_empty() {
        "(no subject)"
    } else {
        entry.subject()
    };
    InspectionNode::Record(vec![
        (
            "subject".into(),
            InspectionNode::Link {
                label: subject.to_owned(),
                href,
            },
        ),
        ("commit".into(), InspectionNode::Text(commit.to_owned())),
        (
            "author".into(),
            InspectionNode::Text(entry.author().to_owned()),
        ),
        (
            "committed".into(),
            InspectionNode::Text(entry.committed_at().to_owned()),
        ),
    ])
}

fn branch_node(branch: &CommittedBranch) -> InspectionNode {
    InspectionNode::Link {
        label: branch.name().to_owned(),
        href: format!("/tree/{}/", branch.commit().as_str()),
    }
}

fn tree_page(root: &Path, suffix: &str) -> Response {
    let (object_id, encoded_path) = suffix.split_once('/').unwrap_or((suffix, ""));
    let Ok(repository) = Repository::discover(root) else {
        return unavailable_response();
    };
    let Some(commit) = resolve_listing_commit(&repository, object_id) else {
        return not_found_response();
    };
    let Ok(decoded_path) = percent_decode(encoded_path) else {
        return bad_request_response();
    };
    let directory = decoded_path.trim_end_matches('/');
    let directory = if directory.is_empty() {
        String::new()
    } else {
        match ManagedPath::new(directory) {
            Ok(path) => path.as_path().to_string_lossy().into_owned(),
            Err(_) => return bad_request_response(),
        }
    };
    let Ok(entries) = repository.list_committed_tree(&commit, MAX_LISTING_TREE_ENTRIES) else {
        return unavailable_response();
    };
    let children = tree_children(&entries, &directory);
    if !directory.is_empty() && children.is_empty() {
        return not_found_response();
    }
    let rows = children
        .iter()
        .map(|child| tree_child_node(&commit, &directory, child))
        .collect::<Vec<_>>();
    let content = if rows.is_empty() {
        InspectionNode::Text("This commit has no files in this tree.".into())
    } else {
        InspectionNode::List(rows)
    };
    let title = if directory.is_empty() {
        "Tree".to_owned()
    } else {
        directory.clone()
    };
    let breadcrumbs = tree_breadcrumbs(&commit, &directory);
    render_inspection_document(&title, &breadcrumbs, &content)
}

fn resolve_listing_commit(repository: &Repository, object_id: &str) -> Option<GitCommitRef> {
    if !matches!(object_id.len(), 40 | 64)
        || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    repository.resolve_snapshot(object_id).ok()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TreeChildKind {
    Directory,
    File { executable: bool },
    Symlink,
    Submodule,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TreeChild {
    name: String,
    kind: TreeChildKind,
}

fn tree_children(
    entries: &[orna_repository_v1::CommittedTreeEntry],
    directory: &str,
) -> Vec<TreeChild> {
    let prefix = if directory.is_empty() {
        String::new()
    } else {
        format!("{directory}/")
    };
    let mut children = std::collections::BTreeMap::<String, TreeChildKind>::new();
    for entry in entries {
        let Some(path) = entry.path().as_path().to_str() else {
            continue;
        };
        let Some(relative) = path.strip_prefix(&prefix) else {
            continue;
        };
        if relative.is_empty() {
            continue;
        }
        let (name, kind) = match relative.split_once('/') {
            Some((name, _)) => (name, TreeChildKind::Directory),
            None => (
                relative,
                match entry.kind() {
                    CommittedTreeEntryKind::File { executable } => {
                        TreeChildKind::File { executable }
                    }
                    CommittedTreeEntryKind::Symlink => TreeChildKind::Symlink,
                    CommittedTreeEntryKind::Submodule => TreeChildKind::Submodule,
                },
            ),
        };
        children.entry(name.to_owned()).or_insert(kind);
    }
    let mut children = children
        .into_iter()
        .map(|(name, kind)| TreeChild { name, kind })
        .collect::<Vec<_>>();
    children.sort_by(|left, right| {
        tree_child_rank(&left.kind)
            .cmp(&tree_child_rank(&right.kind))
            .then_with(|| left.name.cmp(&right.name))
    });
    children
}

const fn tree_child_rank(kind: &TreeChildKind) -> u8 {
    match kind {
        TreeChildKind::Directory => 0,
        TreeChildKind::File { .. } => 1,
        TreeChildKind::Symlink => 2,
        TreeChildKind::Submodule => 3,
    }
}

fn tree_child_node(commit: &GitCommitRef, directory: &str, child: &TreeChild) -> InspectionNode {
    let path = if directory.is_empty() {
        child.name.clone()
    } else {
        format!("{directory}/{}", child.name)
    };
    let (label, kind) = match child.kind {
        TreeChildKind::Directory => {
            let href = format!("/tree/{}/{}/", commit.as_str(), percent_encode_path(&path));
            (
                InspectionNode::Link {
                    label: format!("{}/", child.name),
                    href,
                },
                "directory",
            )
        }
        TreeChildKind::File { executable } => {
            let href = format!("/blob/{}/{}", commit.as_str(), percent_encode_path(&path));
            (
                InspectionNode::Link {
                    label: child.name.clone(),
                    href,
                },
                if executable {
                    "executable file"
                } else {
                    "file"
                },
            )
        }
        TreeChildKind::Symlink => (InspectionNode::Text(child.name.clone()), "symbolic link"),
        TreeChildKind::Submodule => (InspectionNode::Text(child.name.clone()), "submodule"),
    };
    InspectionNode::Record(vec![
        ("name".into(), label),
        ("kind".into(), InspectionNode::Text(kind.into())),
    ])
}

fn tree_breadcrumbs(commit: &GitCommitRef, directory: &str) -> Vec<Breadcrumb> {
    let commit_id = commit.as_str();
    let commit_label = &commit_id[..12];
    let mut breadcrumbs = vec![
        Breadcrumb {
            label: "Commits".into(),
            href: "/".into(),
        },
        Breadcrumb {
            label: commit_label.into(),
            href: format!("/tree/{commit_id}/"),
        },
    ];
    let mut prefix = String::new();
    let parts = directory
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for (index, part) in parts.iter().enumerate() {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        breadcrumbs.push(Breadcrumb {
            label: (*part).to_owned(),
            href: format!("/tree/{commit_id}/{}/", percent_encode_path(&prefix)),
        });
        if index + 1 == parts.len() {
            break;
        }
    }
    breadcrumbs
}

fn blob_page(root: &Path, suffix: &str) -> Response {
    let Some((object_id, encoded_path)) = suffix.split_once('/') else {
        return bad_request_response();
    };
    let Ok(repository) = Repository::discover(root) else {
        return unavailable_response();
    };
    let Some(commit) = resolve_listing_commit(&repository, object_id) else {
        return not_found_response();
    };
    let Ok(decoded_path) = percent_decode(encoded_path) else {
        return bad_request_response();
    };
    let Ok(path) = ManagedPath::new(&decoded_path) else {
        return bad_request_response();
    };
    let Ok(bytes) = repository.read_committed_file(&commit, path.as_path(), MAX_LISTING_FILE_BYTES)
    else {
        return not_found_response();
    };
    let path = path.as_path().to_string_lossy().into_owned();
    let (content_type, content) = match std::str::from_utf8(&bytes) {
        Ok(source)
            if source.chars().all(|character| {
                !character.is_control() || matches!(character, '\n' | '\r' | '\t')
            }) =>
        {
            ("UTF-8 text", InspectionNode::Code(source.to_owned()))
        }
        _ => (
            "binary",
            InspectionNode::Text(format!("Binary content ({} bytes).", bytes.len())),
        ),
    };
    let node = InspectionNode::Record(vec![
        ("path".into(), InspectionNode::Text(path.clone())),
        (
            "content type".into(),
            InspectionNode::Text(content_type.into()),
        ),
        (
            "size".into(),
            InspectionNode::Text(format!("{} bytes", bytes.len())),
        ),
        ("content".into(), content),
    ]);
    let breadcrumbs = file_breadcrumbs(&commit, &path);
    render_inspection_document(&path, &breadcrumbs, &node)
}

fn file_breadcrumbs(commit: &GitCommitRef, path: &str) -> Vec<Breadcrumb> {
    let commit_id = commit.as_str();
    let commit_label = &commit_id[..12];
    let mut breadcrumbs = vec![
        Breadcrumb {
            label: "Commits".into(),
            href: "/".into(),
        },
        Breadcrumb {
            label: commit_label.into(),
            href: format!("/tree/{commit_id}/"),
        },
    ];
    let mut prefix = String::new();
    let parts = path
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        breadcrumbs.push(Breadcrumb {
            label: (*part).to_owned(),
            href: format!("/tree/{commit_id}/{}/", percent_encode_path(&prefix)),
        });
    }
    breadcrumbs
}

fn render_inspection_document(
    title: &str,
    breadcrumbs: &[Breadcrumb],
    content: &InspectionNode,
) -> Response {
    let mut page = String::from(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>",
    );
    page.push_str(&html_escape(title));
    page.push_str("</title>");
    page.push_str(LISTING_STYLE);
    page.push_str("</head><body>");
    if !breadcrumbs.is_empty() {
        page.push_str("<nav aria-label=\"Breadcrumb\"><ol>");
        for breadcrumb in breadcrumbs {
            page.push_str("<li><a href=\"");
            page.push_str(&html_escape(&breadcrumb.href));
            page.push_str("\">");
            page.push_str(&html_escape(&breadcrumb.label));
            page.push_str("</a></li>");
        }
        page.push_str("</ol></nav>");
    }
    page.push_str("<main><h1>");
    page.push_str(&html_escape(title));
    page.push_str("</h1>");
    render_inspection_node(content, &mut page);
    page.push_str("</main></body></html>");
    Response::new(200, "text/html; charset=utf-8", page.into_bytes())
}

fn render_home_document(identity: RuntimeIdentity, content: &InspectionNode) -> Response {
    let database = format_uuid(identity.database_id);
    let mut page = String::from(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Orna</title>",
    );
    page.push_str(LISTING_STYLE);
    page.push_str("</head><body><main><h1>Orna database</h1>");
    render_inspection_node(content, &mut page);
    page.push_str(&format!(
        "<section id=\"live-repl\" data-database=\"{}\"><h2>Run the selected Orna source</h2><label for=\"repl-source\">Orna input</label><textarea id=\"repl-source\" rows=\"4\" spellcheck=\"false\">serve()</textarea><button id=\"repl-run\" type=\"button\" disabled>Run</button><p id=\"repl-status\" aria-live=\"polite\">Connecting to the runtime…</p><pre id=\"repl-events\">No run yet.</pre></section><section aria-labelledby=\"live-title\"><h2 id=\"live-title\">Live presentation</h2><p id=\"live-status\" role=\"status\" aria-live=\"polite\">Connecting to the database…</p><div id=\"live-presentation\"></div></section>",
        html_escape(&database)
    ));
    page.push_str(&format!(
        "</main><script type=\"application/json\" id=\"run-events-source\">{}</script><script type=\"module\" src=\"/playground/assets/serve-home.mjs\"></script></body></html>",
        json_string(LIVE_RUN_EVENTS_WATCH_SOURCE)
    ));
    Response::new(200, "text/html; charset=utf-8", page.into_bytes())
}

fn render_inspection_node(node: &InspectionNode, page: &mut String) {
    match node {
        InspectionNode::Text(value) => page.push_str(&html_escape(value)),
        InspectionNode::Link { label, href } => {
            page.push_str("<a href=\"");
            page.push_str(&html_escape(href));
            page.push_str("\">");
            page.push_str(&html_escape(label));
            page.push_str("</a>");
        }
        InspectionNode::Record(fields) => {
            page.push_str("<dl>");
            for (label, value) in fields {
                page.push_str("<dt>");
                page.push_str(&html_escape(label));
                page.push_str("</dt><dd>");
                render_inspection_node(value, page);
                page.push_str("</dd>");
            }
            page.push_str("</dl>");
        }
        InspectionNode::List(values) | InspectionNode::OrderedList(values) => {
            let (open, close) = if matches!(node, InspectionNode::OrderedList(_)) {
                ("<ol>", "</ol>")
            } else {
                ("<ul>", "</ul>")
            };
            page.push_str(open);
            for value in values {
                page.push_str("<li>");
                render_inspection_node(value, page);
                page.push_str("</li>");
            }
            page.push_str(close);
        }
        InspectionNode::Code(source) => {
            page.push_str("<pre><code>");
            page.push_str(&html_escape(source));
            page.push_str("</code></pre>");
        }
    }
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
            let high = input
                .get(index + 1)
                .and_then(|byte| hex_digit(*byte))
                .ok_or(())?;
            let low = input
                .get(index + 2)
                .and_then(|byte| hex_digit(*byte))
                .ok_or(())?;
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
            return Err(io::Error::other(
                "Git HTTP backend returned malformed headers",
            ));
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

fn clone_report(root: &Path, identity: RuntimeIdentity) -> io::Result<Vec<u8>> {
    let repository =
        Repository::discover(root).map_err(|_| io::Error::other("clone is unavailable"))?;
    let head = repository
        .head()
        .map_err(|_| io::Error::other("clone HEAD is unavailable"))?;
    let cwd = repository
        .cwd_generation(RuntimeGeneration::new(0))
        .map_err(|_| io::Error::other("clone CWD is unavailable"))?;
    let head_json = head
        .as_ref()
        .map_or_else(|| "null".to_owned(), |head| json_string(head.as_str()));
    let branch_json = cwd.branch().map_or_else(|| "null".to_owned(), json_string);
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
            || name
                .bytes()
                .any(|byte| !byte.is_ascii_alphanumeric() && byte != b'-')
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
    Response::new(
        404,
        "application/json",
        br#"{"error":"not_found"}"#.to_vec(),
    )
}

fn bad_request_response() -> Response {
    Response::new(
        400,
        "application/json",
        br#"{"error":"bad_request"}"#.to_vec(),
    )
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
    use orna_live_v1::LiveApplication;
    use std::process::Command;

    const SERVE_FIXTURE: &str = include_str!("../tests/fixtures/serve-no-autoload.orna");
    const SERVE_ENTRY_FIXTURE: &str = include_str!("../tests/fixtures/serve-entry.orna");

    fn git_succeeds(directory: &Path, arguments: &[&str]) {
        let output = Command::new("git")
            .args(arguments)
            .current_dir(directory)
            .output()
            .expect("run Git");
        assert!(
            output.status.success(),
            "Git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_fixture_rows(directory: &Path, table: &str, rows: &[(&str, &str)]) {
        let table_directory = directory.join("playground").join(table);
        std::fs::create_dir_all(&table_directory).expect("create playground row directory");
        for (id, source) in rows {
            std::fs::write(table_directory.join(format!("{id}.orna")), source)
                .expect("write crate-local playground row fixture");
        }
    }

    #[test]
    fn selected_serve_entry_runs_through_the_live_evaluator_and_returns_a_typed_result() {
        let mut state = new_serve_state(
            PathBuf::new(),
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            "127.0.0.1:9000".parse().expect("loopback address"),
            orna_semantic_v1::Catalogue::authoritative_core(),
        )
        .expect("serve state");
        let request = [4; 16];
        let fingerprint = [5; 32];
        let message = Message::Eval {
            source: SERVE_ENTRY_FIXTURE.to_owned(),
            database: orna_protocol_v1::DatabaseContext {
                database: [1; 16],
                snapshot: None,
            },
            presentation: PresentationContext {
                locale: "en".into(),
                timezone: None,
                width: None,
                theme: "terminal/default".into(),
                supported_kinds: vec!["text".into(), "group".into()],
            },
            fingerprint,
        };

        let response = state
            .application
            .eval([3; 16], request, &message)
            .expect("selected serve source evaluates");
        assert_eq!(response.request, Some(request));
        assert_eq!(response.watch, None);
        let Message::Result {
            status,
            value,
            fingerprint: result_fingerprint,
            diagnostic,
        } = response.message
        else {
            panic!("Run returns a typed Result message");
        };
        assert_eq!(status, orna_protocol_v1::ResultStatus::Success);
        assert_eq!(result_fingerprint, fingerprint);
        assert_eq!(diagnostic, None);
        assert_eq!(
            value.expect("typed Run result").raw(),
            &orna_foundation_v1::OvbRaw::Int(42.into())
        );
    }

    fn listing_page(root: &Path, path: &str) -> Response {
        host_route(
            root,
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            &Request {
                method: "GET".into(),
                path: path.into(),
                query: String::new(),
                headers: Vec::new(),
                body: Vec::new(),
            },
        )
    }

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
        let home = String::from_utf8(home.body).expect("commit listing HTML");
        assert!(home.contains("No commits in this repository."));
        assert!(home.contains("Open the Orna playground"));
        assert!(home.contains("href=\"/playground/\""));
        assert!(home.contains("id=\"live-repl\""));
        assert!(home.contains("id=\"repl-source\""));
        assert!(home.contains(">serve()</textarea>"));
        assert!(home.contains("id=\"live-presentation\""));
        assert!(home.contains("id=\"run-events-source\""));
        assert!(home.contains("src=\"/playground/assets/serve-home.mjs\""));
        assert!(home.contains("orna/serve/run-events/v1"));
        assert!(!home.contains("new WebSocket(endpoint"));
        assert!(!home.contains("/api/query"));
        assert!(!home.contains("wasm"));
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
        assert_eq!(query.status, 404);
    }

    #[test]
    fn playground_loads_committed_sample_and_asset_rows_from_the_database() {
        const PLAYGROUND_SAMPLE: &str = include_str!("../tests/fixtures/playground-sample.orna");
        const PLAYGROUND_SCHEMA: &str = include_str!("../tests/fixtures/playground-schema.orna");
        const LEGACY_EXAMPLE: &str = include_str!("../tests/fixtures/playground-example.orna");
        const ASSET_INDEX: &str = include_str!("../tests/fixtures/playground-asset-index.orna");
        const ASSET_APP: &str = include_str!("../tests/fixtures/playground-asset-app.orna");
        const ASSET_STYLE: &str = include_str!("../tests/fixtures/playground-asset-style.orna");
        const ASSET_PRESENTATION: &str =
            include_str!("../tests/fixtures/playground-asset-presentation.orna");
        const ASSET_HOME: &str = include_str!("../tests/fixtures/playground-asset-home.orna");
        const ASSET_PLAYGROUND: &str =
            include_str!("../tests/fixtures/playground-asset-playground.orna");
        const ASSET_LSP_JS: &str = include_str!("../tests/fixtures/playground-asset-lsp-js.orna");
        const ASSET_LSP_WASM: &str =
            include_str!("../tests/fixtures/playground-asset-lsp-wasm.orna");
        const ASSET_UNCOMMITTED: &str =
            include_str!("../tests/fixtures/playground-asset-uncommitted.orna");
        const ASSET_STALE_INDEX: &str =
            include_str!("../tests/fixtures/playground-asset-index-stale.orna");
        const ROUTES: [(&str, &str); 11] = [
            (
                "route-2f706c617967726f756e642f",
                include_str!("../tests/fixtures/playground-route-page.orna"),
            ),
            (
                "route-2f706c617967726f756e642f656d626564",
                include_str!("../tests/fixtures/playground-route-embed.orna"),
            ),
            (
                "route-2f706c617967726f756e642f6173736574732f6170702e6a73",
                include_str!("../tests/fixtures/playground-route-app.orna"),
            ),
            (
                "route-2f706c617967726f756e642f6173736574732f7374796c652e637373",
                include_str!("../tests/fixtures/playground-route-style.orna"),
            ),
            (
                "route-2f706c617967726f756e642f6173736574732f6f726e612d656469746f722d636f6e6669672e6a736f6e",
                include_str!("../tests/fixtures/playground-route-config.orna"),
            ),
            (
                "route-2f706c617967726f756e642f6173736574732f656d6265642e6a73",
                include_str!("../tests/fixtures/playground-route-embed-script.orna"),
            ),
            (
                "route-2f706c617967726f756e642f6173736574732f70726573656e746174696f6e2e6d6a73",
                include_str!("../tests/fixtures/playground-route-presentation.orna"),
            ),
            (
                "route-2f706c617967726f756e642f6173736574732f73657276652d686f6d652e6d6a73",
                include_str!("../tests/fixtures/playground-route-home-runtime.orna"),
            ),
            (
                "route-2f706c617967726f756e642f6173736574732f73657276652d706c617967726f756e642e6d6a73",
                include_str!("../tests/fixtures/playground-route-playground-runtime.orna"),
            ),
            (
                "route-2f706c617967726f756e642f6173736574732f6c73702d7761736d2f6f726e615f6c73702e6a73",
                include_str!("../tests/fixtures/playground-route-lsp-js.orna"),
            ),
            (
                "route-2f706c617967726f756e642f6173736574732f6c73702d7761736d2f6f726e615f6c73705f62672e7761736d",
                include_str!("../tests/fixtures/playground-route-lsp-wasm.orna"),
            ),
        ];
        const ENTRIES: [(&str, &str); 11] = [
            (
                "entry-page",
                include_str!("../tests/fixtures/playground-entry-page.orna"),
            ),
            (
                "entry-embed",
                include_str!("../tests/fixtures/playground-entry-embed.orna"),
            ),
            (
                "entry-app",
                include_str!("../tests/fixtures/playground-entry-app.orna"),
            ),
            (
                "entry-style",
                include_str!("../tests/fixtures/playground-entry-style.orna"),
            ),
            (
                "entry-config",
                include_str!("../tests/fixtures/playground-entry-config.orna"),
            ),
            (
                "entry-embed-script",
                include_str!("../tests/fixtures/playground-entry-embed-script.orna"),
            ),
            (
                "entry-presentation",
                include_str!("../tests/fixtures/playground-entry-presentation.orna"),
            ),
            (
                "entry-home-runtime",
                include_str!("../tests/fixtures/playground-entry-home-runtime.orna"),
            ),
            (
                "entry-playground-runtime",
                include_str!("../tests/fixtures/playground-entry-playground-runtime.orna"),
            ),
            (
                "entry-lsp-js",
                include_str!("../tests/fixtures/playground-entry-lsp-js.orna"),
            ),
            (
                "entry-lsp-wasm",
                include_str!("../tests/fixtures/playground-entry-lsp-wasm.orna"),
            ),
        ];
        let directory = tempfile::tempdir().expect("temporary database");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=playground"],
        );
        std::fs::write(directory.path().join("playground.orna"), PLAYGROUND_SCHEMA)
            .expect("write Sample table");
        let sample_path = directory.path().join("playground/Sample/hello.orna");
        std::fs::create_dir_all(sample_path.parent().expect("sample table directory"))
            .expect("create Sample rows directory");
        std::fs::write(&sample_path, PLAYGROUND_SAMPLE).expect("write Sample row");
        let legacy_path = directory.path().join("playground/examples/legacy.orna");
        std::fs::create_dir_all(legacy_path.parent().expect("example directory"))
            .expect("create examples directory");
        std::fs::write(&legacy_path, LEGACY_EXAMPLE).expect("write committed file example");
        let asset_directory = directory.path().join("playground/Asset");
        std::fs::create_dir_all(&asset_directory).expect("create Asset rows directory");
        std::fs::write(
            asset_directory.join("asset-696e6465782e68746d6c.orna"),
            ASSET_INDEX,
        )
        .expect("write database shell asset");
        std::fs::write(
            asset_directory.join("asset-6173736574732f6170702e6a73.orna"),
            ASSET_APP,
        )
        .expect("write database JavaScript asset");
        std::fs::write(
            asset_directory.join("asset-6173736574732f7374796c652e637373.orna"),
            ASSET_STYLE,
        )
        .expect("write database stylesheet asset");
        write_fixture_rows(directory.path(), "Route", &ROUTES);
        write_fixture_rows(directory.path(), "Entry", &ENTRIES);
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
        ] {
            std::fs::write(asset_directory.join(format!("asset-{id}.orna")), source)
                .expect("write committed browser support asset");
        }
        git_succeeds(
            directory.path(),
            &[
                "add",
                "playground.orna",
                "playground/Sample/hello.orna",
                "playground/examples/legacy.orna",
                "playground/Asset",
                "playground/Route",
                "playground/Entry",
            ],
        );
        git_succeeds(
            directory.path(),
            &[
                "-c",
                "user.name=kierandrewett",
                "-c",
                "user.email=kieran@drewett.dev",
                "commit",
                "--quiet",
                "-m",
                "add playground sample record",
            ],
        );
        std::fs::write(
            directory.path().join("playground/Sample/uncommitted.orna"),
            PLAYGROUND_SAMPLE,
        )
        .expect("write uncommitted Sample row");
        std::fs::write(
            directory
                .path()
                .join("playground/examples/uncommitted.orna"),
            "99 + 1",
        )
        .expect("write uncommitted file example");
        std::fs::write(
            asset_directory.join("asset-6173736574732f756e636f6d6d69747465642e6a73.orna"),
            ASSET_UNCOMMITTED,
        )
        .expect("write uncommitted database asset");

        let examples = listing_page(directory.path(), "/api/examples");
        assert_eq!(examples.status, 200);
        let examples: serde_json::Value =
            serde_json::from_slice(&examples.body).expect("example records JSON");
        let revision = examples["revision"].as_str().expect("committed revision");
        assert_eq!(revision.len(), 40);
        assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(examples["examples"].as_array().map(Vec::len), Some(2));
        let examples = examples["examples"].as_array().expect("example list");
        let sample = examples
            .iter()
            .find(|example| example["path"] == "playground/Sample/hello.orna")
            .expect("database record example");
        assert_eq!(sample["name"], "Hello, Orna");
        assert_eq!(
            sample["source"],
            "pub fn double(value: Int): Int = value + value;\ndouble(21)"
        );
        let legacy = examples
            .iter()
            .find(|example| example["path"] == "playground/examples/legacy.orna")
            .expect("committed file example");
        assert_eq!(legacy["name"], "legacy");
        assert_eq!(legacy["source"], LEGACY_EXAMPLE);

        let identity = RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        };
        let page = playground_asset(directory.path(), identity, "/playground/");
        assert_eq!(page.status, 200);
        assert_eq!(page.content_type, "text/html; charset=utf-8");
        let page = String::from_utf8(page.body).expect("database page UTF-8");
        assert!(page.contains("<main>database shell"));
        assert!(page.contains("id=\"style-reload-status\""));
        assert!(page.contains(&format!(
            "data-database=\"{}\"",
            format_uuid(identity.database_id)
        )));
        assert!(page.contains("id=\"live-bridge\""));
        assert!(page.contains("id=\"live-presentation\""));
        assert!(page.contains("id=\"run-events-source\""));
        assert!(page.contains("src=\"/playground/assets/serve-playground.mjs\""));
        assert!(page.contains("\\u0000orna/serve/run-events/v1"));
        let runtime = listing_page(directory.path(), "/playground/assets/serve-playground.mjs");
        assert_eq!(runtime.status, 200);
        assert!(
            String::from_utf8(runtime.body)
                .expect("playground runtime UTF-8")
                .contains("globalThis.ornaPlaygroundRun")
        );
        let presentation = listing_page(directory.path(), "/playground/assets/presentation.mjs");
        assert_eq!(presentation.status, 200);
        assert!(
            String::from_utf8(presentation.body)
                .unwrap()
                .contains("LivePresentation")
        );
        let lsp_binding = listing_page(directory.path(), "/playground/assets/lsp-wasm/orna_lsp.js");
        assert_eq!(lsp_binding.status, 200);
        assert_eq!(lsp_binding.body, b"export default async function init() {}");
        let lsp_wasm = listing_page(
            directory.path(),
            "/playground/assets/lsp-wasm/orna_lsp_bg.wasm",
        );
        assert_eq!(lsp_wasm.status, 200);
        assert_eq!(lsp_wasm.content_type, "application/wasm");
        assert_eq!(lsp_wasm.body, b"\0asm\x01\0\0\0");
        let embed = playground_asset(directory.path(), identity, "/playground/embed");
        assert_eq!(embed.status, 200);
        assert!(embed.headers.iter().any(|(name, value)| {
            name == "Content-Security-Policy" && value == "frame-ancestors *"
        }));
        let embed = String::from_utf8(embed.body).expect("embedded page UTF-8");
        assert!(embed.contains("<header data-page-header hidden>"));
        assert!(embed.contains("<main>database shell"));
        assert!(embed.contains("id=\"style-reload-status\""));
        let embed_with_slash = playground_asset(directory.path(), identity, "/playground/embed/");
        assert_eq!(embed_with_slash.status, 200);
        let script = playground_asset(directory.path(), identity, "/playground/assets/app.js");
        assert_eq!(script.status, 200);
        assert!(
            script
                .headers
                .iter()
                .any(|(name, value)| name == "X-Content-Type-Options" && value == "nosniff")
        );
        assert_eq!(script.content_type, "text/javascript; charset=utf-8");
        assert_eq!(script.body, b"globalThis.ornaPlaygroundReady = true;");
        let stylesheet =
            playground_asset(directory.path(), identity, "/playground/assets/style.css");
        assert_eq!(stylesheet.status, 200);
        assert_eq!(stylesheet.content_type, "text/css; charset=utf-8");
        assert_eq!(stylesheet.body, b"body { color: #202122; }");
        assert!(!directory.path().join("playground/web-ui/dist").exists());
        let uncommitted_asset = playground_asset(
            directory.path(),
            identity,
            "/playground/assets/uncommitted.js",
        );
        assert_eq!(uncommitted_asset.status, 404);

        let traversal =
            playground_asset(directory.path(), identity, "/playground/%2e%2e/README.txt");
        assert_eq!(traversal.status, 400);

        std::fs::write(
            asset_directory.join("asset-696e6465782e68746d6c.orna"),
            ASSET_STALE_INDEX,
        )
        .expect("replace the shell row with one lacking the embed marker");
        git_succeeds(directory.path(), &["add", "playground/Asset"]);
        git_succeeds(
            directory.path(),
            &[
                "-c",
                "user.name=kierandrewett",
                "-c",
                "user.email=kieran@drewett.dev",
                "commit",
                "--quiet",
                "-m",
                "replace playground asset record",
            ],
        );
        let stale_embed = playground_asset(directory.path(), identity, "/playground/embed");
        assert_eq!(stale_embed.status, 503);
    }

    #[test]
    fn playground_sample_records_match_the_declared_table_and_filename_key() {
        const PLAYGROUND_SAMPLE: &str = include_str!("../tests/fixtures/playground-sample.orna");
        const PLAYGROUND_SCHEMA: &str = include_str!("../tests/fixtures/playground-schema.orna");

        assert!(has_playground_sample_table(PLAYGROUND_SCHEMA));
        assert_eq!(
            decode_playground_sample(PLAYGROUND_SAMPLE, "hello"),
            Some((
                "Hello, Orna".into(),
                "pub fn double(value: Int): Int = value + value;\ndouble(21)".into(),
            ))
        );
        assert_eq!(decode_playground_sample(PLAYGROUND_SAMPLE, "other"), None);
        assert_eq!(
            decode_playground_sample(
                "{ id: \"hello\", id: \"hello\", name: \"Hello\", source: \"1\" }",
                "hello"
            ),
            None
        );
        assert!(!has_playground_sample_table(
            "pub table Sample(id: Int) { name: Str, source: Str }"
        ));
    }

    #[test]
    fn playground_asset_records_match_the_declared_table_and_path_key() {
        const PLAYGROUND_SCHEMA: &str = include_str!("../tests/fixtures/playground-schema.orna");
        const ASSET_APP: &str = include_str!("../tests/fixtures/playground-asset-app.orna");
        const ASSET_PRESENTATION: &str =
            include_str!("../tests/fixtures/playground-asset-presentation.orna");
        const ASSET_STYLE: &str = include_str!("../tests/fixtures/playground-asset-style.orna");
        const ROUTE_PAGE: &str = include_str!("../tests/fixtures/playground-route-page.orna");
        const ENTRY_PAGE: &str = include_str!("../tests/fixtures/playground-entry-page.orna");

        assert!(has_playground_asset_table(PLAYGROUND_SCHEMA));
        assert!(has_playground_route_table(PLAYGROUND_SCHEMA));
        assert!(has_playground_entry_table(PLAYGROUND_SCHEMA));
        let parsed_presentation = parse_row(ASSET_PRESENTATION);
        assert!(
            parsed_presentation.is_ok(),
            "presentation asset row parse: {:#?}",
            parsed_presentation.diagnostics
        );
        assert_eq!(
            decode_playground_asset(ASSET_APP, "asset-6173736574732f6170702e6a73"),
            Some((
                "assets/app.js".into(),
                "text/javascript; charset=utf-8".into(),
                "globalThis.ornaPlaygroundReady = true;".into(),
            ))
        );
        assert_eq!(
            decode_playground_asset(
                ASSET_PRESENTATION,
                "asset-6173736574732f70726573656e746174696f6e2e6d6a73"
            ),
            Some((
                "assets/presentation.mjs".into(),
                "text/javascript; charset=utf-8".into(),
                "export class LivePresentation {}".into(),
            ))
        );
        assert_eq!(decode_playground_asset(ASSET_APP, "different-id"), None);
        assert_eq!(
            decode_playground_asset(ASSET_STYLE, "asset-6173736574732f7374796c652e637373"),
            Some((
                "assets/style.css".into(),
                "text/css; charset=utf-8".into(),
                "body { color: #202122; }".into(),
            ))
        );
        assert!(!has_playground_asset_table(
            "pub table Asset(id: Int) { path: Str, media_type: Str, content: Str }"
        ));
        assert!(!has_playground_route_table(
            "pub table Route(id: Int) { path: Str, entry: Str }"
        ));
        assert!(!has_playground_entry_table(
            "pub table Entry(id: Str) { asset_path: Int, kind: Str }"
        ));
        assert_eq!(
            decode_playground_route(ROUTE_PAGE, "route-2f706c617967726f756e642f"),
            Some(("/playground/".into(), "entry-page".into()))
        );
        assert_eq!(decode_playground_route(ROUTE_PAGE, "different-id"), None);
        assert_eq!(
            decode_playground_entry(ENTRY_PAGE, "entry-page"),
            Some(("index.html".into(), PlaygroundEntryKind::Page))
        );
        assert_eq!(decode_playground_entry(ENTRY_PAGE, "different-id"), None);
        assert_eq!(
            normalize_playground_route_path("/playground"),
            Some("/playground/".into())
        );
        assert_eq!(
            normalize_playground_route_path("/playground/embed/"),
            Some("/playground/embed".into())
        );
        assert_eq!(
            normalize_playground_route_path("/playground/%2e%2e/README.txt"),
            None
        );
    }

    #[test]
    fn playground_catalogue_lists_only_committed_route_rows_that_decode() {
        let directory = tempfile::tempdir().expect("temporary repository");
        git_succeeds(directory.path(), &["init", "--quiet"]);
        let route_directory = directory.path().join("playground/Route");
        std::fs::create_dir_all(&route_directory).expect("create route rows");
        std::fs::write(
            route_directory.join(
                "route-2f706c617967726f756e642f6173736574732f6578616d706c65732e637373.orna",
            ),
            include_str!("../tests/fixtures/playground-route-example-catalog-style.orna"),
        )
        .expect("write valid route row");
        std::fs::write(
            route_directory.join("route-0000000000000000.orna"),
            "{ not a route row }",
        )
        .expect("write invalid route row");
        git_succeeds(directory.path(), &["add", "playground/Route"]);
        git_succeeds(
            directory.path(),
            &[
                "-c",
                "user.name=kierandrewett",
                "-c",
                "user.email=kieran@drewett.dev",
                "commit",
                "--quiet",
                "-m",
                "add catalogue routes",
            ],
        );

        assert_eq!(
            playground_catalogue_routes(directory.path()),
            Ok(vec!["/playground/assets/examples.css".to_owned()])
        );
    }

    #[test]
    fn playground_catalogue_json_returns_committed_route_rows_from_fixtures() {
        let directory = tempfile::tempdir().expect("temporary repository");
        git_succeeds(directory.path(), &["init", "--quiet"]);
        let route_directory = directory.path().join("playground/Route");
        std::fs::create_dir_all(&route_directory).expect("create route rows");
        for (file, source) in [
            (
                "route-2f706c617967726f756e642f6173736574732f6578616d706c65732e637373.orna",
                include_str!("../tests/fixtures/playground-route-example-catalog-style.orna"),
            ),
            (
                "route-2f706c617967726f756e642f6173736574732f6578616d706c65732e6d6a73.orna",
                include_str!("../tests/fixtures/playground-route-catalogue-script.orna"),
            ),
            (
                "route-0000000000000000.orna",
                include_str!("../tests/fixtures/playground-route-catalogue-invalid.orna"),
            ),
        ] {
            std::fs::write(route_directory.join(file), source).expect("write route row");
        }
        git_succeeds(directory.path(), &["add", "playground/Route"]);
        git_succeeds(
            directory.path(),
            &[
                "-c",
                "user.name=kierandrewett",
                "-c",
                "user.email=kieran@drewett.dev",
                "commit",
                "--quiet",
                "-m",
                "add catalogue routes",
            ],
        );

        let response = playground_catalogue_json(directory.path());
        assert_eq!(response.status, 200);
        assert_eq!(response.content_type, "application/json");
        assert_eq!(
            String::from_utf8(response.body).expect("JSON body is UTF-8"),
            r#"{"routes":["/playground/assets/examples.css","/playground/assets/examples.mjs"]}"#
        );
    }

    #[test]
    fn playground_catalogue_json_single_route_row_from_one_fixture() {
        let directory = tempfile::tempdir().expect("temporary repository");
        git_succeeds(directory.path(), &["init", "--quiet"]);
        let route_directory = directory.path().join("playground/Route");
        std::fs::create_dir_all(&route_directory).expect("create route rows");
        std::fs::write(
            route_directory
                .join("route-2f706c617967726f756e642f6173736574732f6578616d706c65732e637373.orna"),
            include_str!("../tests/fixtures/playground-route-example-catalog-style.orna"),
        )
        .expect("write route row");
        git_succeeds(directory.path(), &["add", "playground/Route"]);
        git_succeeds(
            directory.path(),
            &[
                "-c",
                "user.name=kierandrewett",
                "-c",
                "user.email=kieran@drewett.dev",
                "commit",
                "--quiet",
                "-m",
                "add one catalogue route",
            ],
        );

        let response = playground_catalogue_json(directory.path());
        assert_eq!(response.status, 200);
        assert_eq!(
            String::from_utf8(response.body).expect("JSON body is UTF-8"),
            r#"{"routes":["/playground/assets/examples.css"]}"#
        );
    }

    #[test]
    fn playground_catalogue_pages_one_fixture_row_and_rejects_bad_pages() {
        let directory = tempfile::tempdir().expect("temporary repository");
        git_succeeds(directory.path(), &["init", "--quiet"]);
        let route_directory = directory.path().join("playground/Route");
        std::fs::create_dir_all(&route_directory).expect("create route rows");
        std::fs::write(
            route_directory
                .join("route-2f706c617967726f756e642f6173736574732f6578616d706c65732e637373.orna"),
            include_str!("../tests/fixtures/playground-route-catalogue-page.orna"),
        )
        .expect("write route row");
        git_succeeds(directory.path(), &["add", "playground/Route"]);
        git_succeeds(
            directory.path(),
            &[
                "-c",
                "user.name=kierandrewett",
                "-c",
                "user.email=kieran@drewett.dev",
                "commit",
                "--quiet",
                "-m",
                "add one catalogue route for paging",
            ],
        );
        let identity = RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        };

        let first = playground_catalogue_page(directory.path(), identity, "");
        assert_eq!(first.status, 200);
        let first = String::from_utf8(first.body).expect("HTML body is UTF-8");
        assert!(first.contains("/playground/assets/examples.css"));
        assert!(!first.contains("Next page"));

        let past_end = playground_catalogue_page(directory.path(), identity, "page=2");
        assert_eq!(past_end.status, 200);
        let past_end = String::from_utf8(past_end.body).expect("HTML body is UTF-8");
        assert!(!past_end.contains("/playground/assets/examples.css"));

        for query in ["page=0", "page=", "page=x", "size=2"] {
            assert_eq!(
                playground_catalogue_page(directory.path(), identity, query).status,
                400,
                "{query}"
            );
        }
    }

    #[test]
    fn playground_catalogue_empty_result_for_one_undecodable_fixture_row() {
        let directory = tempfile::tempdir().expect("temporary repository");
        git_succeeds(directory.path(), &["init", "--quiet"]);
        let route_directory = directory.path().join("playground/Route");
        std::fs::create_dir_all(&route_directory).expect("create route rows");
        std::fs::write(
            route_directory.join("route-0000000000000000.orna"),
            include_str!("../tests/fixtures/playground-route-catalogue-invalid.orna"),
        )
        .expect("write undecodable route row");
        git_succeeds(directory.path(), &["add", "playground/Route"]);
        git_succeeds(
            directory.path(),
            &[
                "-c",
                "user.name=kierandrewett",
                "-c",
                "user.email=kieran@drewett.dev",
                "commit",
                "--quiet",
                "-m",
                "add an undecodable catalogue route",
            ],
        );

        let json = playground_catalogue_json(directory.path());
        assert_eq!(json.status, 200);
        assert_eq!(
            String::from_utf8(json.body).expect("JSON body is UTF-8"),
            r#"{"routes":[]}"#
        );

        let identity = RuntimeIdentity {
            database_id: [1; 16],
            repository_id: [2; 16],
        };
        let page = playground_catalogue_page(directory.path(), identity, "");
        assert_eq!(page.status, 200);
        let page = String::from_utf8(page.body).expect("HTML body is UTF-8");
        assert!(page.contains("No playground catalogue entries are committed."));
        assert!(!page.contains("Next page"));
    }

    #[test]
    fn playground_example_catalog_rows_decode_for_asset_namespace() {
        const ROUTE: &str =
            include_str!("../tests/fixtures/playground-route-example-catalog-style.orna");
        const ENTRY: &str =
            include_str!("../tests/fixtures/playground-entry-example-catalog-style.orna");
        const ASSET: &str =
            include_str!("../tests/fixtures/playground-asset-example-catalog-style.orna");

        let entry_id = "entry-asset-6173736574732f6578616d706c65732e637373";
        let asset_id = "asset-6173736574732f6578616d706c65732e637373";
        assert_eq!(
            decode_playground_route(
                ROUTE,
                "route-2f706c617967726f756e642f6173736574732f6578616d706c65732e637373",
            ),
            Some(("/playground/assets/examples.css".into(), entry_id.into()))
        );
        assert_eq!(
            decode_playground_entry(ENTRY, entry_id),
            Some(("assets/examples.css".into(), PlaygroundEntryKind::Asset))
        );
        let parsed_asset = parse_row(ASSET);
        assert!(
            parsed_asset.is_ok(),
            "catalog stylesheet row parse: {:#?}",
            parsed_asset.diagnostics
        );
        assert_eq!(
            decode_playground_asset(ASSET, asset_id),
            Some((
                "assets/examples.css".into(),
                "text/css; charset=utf-8".into(),
                "body { color: #202122; }".into(),
            ))
        );
    }

    #[test]
    fn embedded_page_requires_one_header_marker_on_the_header_tag() {
        let mut missing = "<html><body><header>Title</header></body></html>".to_owned();
        assert!(!hide_embedded_page_header(&mut missing));
        assert_eq!(missing, "<html><body><header>Title</header></body></html>");

        let mut wrong_tag = "<html><body><main data-page-header></main></body></html>".to_owned();
        assert!(!hide_embedded_page_header(&mut wrong_tag));

        let mut duplicate =
            "<header data-page-header>One</header><header data-page-header>Two</header>".to_owned();
        assert!(!hide_embedded_page_header(&mut duplicate));
        assert_eq!(
            duplicate,
            "<header data-page-header>One</header><header data-page-header>Two</header>"
        );
    }

    #[test]
    fn default_listing_reads_commits_trees_and_files_from_git() {
        let directory = tempfile::tempdir().expect("temporary database");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=listing"],
        );
        std::fs::create_dir_all(directory.path().join("src")).expect("source directory");
        std::fs::write(directory.path().join("main.orna"), SERVE_FIXTURE)
            .expect("crate-local Orna source fixture");
        std::fs::write(directory.path().join("src/nested.orna"), SERVE_FIXTURE)
            .expect("nested Orna source fixture");
        std::fs::write(directory.path().join("README.txt"), "committed text\n")
            .expect("generic text file");
        std::fs::write(directory.path().join("src/data.bin"), [0, 255])
            .expect("generic binary file");
        git_succeeds(
            directory.path(),
            &["add", "--", "main.orna", "src", "README.txt"],
        );
        git_succeeds(
            directory.path(),
            &[
                "-c",
                "user.name=kierandrewett",
                "-c",
                "user.email=kieran@drewett.dev",
                "commit",
                "--quiet",
                "-m",
                "render <listing> safely",
            ],
        );
        git_succeeds(directory.path(), &["branch", "archive"]);
        std::fs::write(directory.path().join("uncommitted.txt"), "not in Git yet")
            .expect("uncommitted worktree file");
        let repository = Repository::discover(directory.path()).expect("database repository");
        let commit = repository
            .head()
            .expect("read HEAD")
            .expect("listing commit");
        let commit_id = commit.as_str();

        let log = listing_page(directory.path(), "/");
        assert_eq!(log.status, 200);
        assert_eq!(log.content_type, "text/html; charset=utf-8");
        let log = String::from_utf8(log.body).expect("commit log HTML");
        assert!(log.contains("render &lt;listing&gt; safely"));
        assert!(log.contains(&format!("/tree/{commit_id}/")));
        assert!(log.contains("Branches"));
        assert!(log.contains("archive"));
        assert!(log.contains("listing"));
        assert!(log.contains("kierandrewett"));
        assert!(!log.contains("uncommitted.txt"));
        assert!(log.contains("href=\"/playground/\""));
        assert!(!log.contains("<header"));

        let tree = listing_page(directory.path(), &format!("/tree/{commit_id}/"));
        assert_eq!(tree.status, 200);
        let tree = String::from_utf8(tree.body).expect("tree HTML");
        assert!(tree.contains(&format!("/tree/{commit_id}/src/")));
        assert!(tree.contains(&format!("/blob/{commit_id}/main.orna")));
        assert!(tree.contains(&format!("/blob/{commit_id}/README.txt")));
        assert!(!tree.contains("uncommitted.txt"));

        let nested_tree = listing_page(directory.path(), &format!("/tree/{commit_id}/src/"));
        assert_eq!(nested_tree.status, 200);
        let nested_tree = String::from_utf8(nested_tree.body).expect("nested tree HTML");
        assert!(nested_tree.contains(&format!("/blob/{commit_id}/src/nested.orna")));
        assert!(nested_tree.contains(&format!("/blob/{commit_id}/src/data.bin")));

        let source = listing_page(
            directory.path(),
            &format!("/blob/{commit_id}/src/nested.orna"),
        );
        assert_eq!(source.status, 200);
        let source = String::from_utf8(source.body).expect("Orna source HTML");
        assert!(source.contains("pub fn main(): Int = 1 / 0;"));
        assert!(source.contains("&lt;script&gt;alert(&#39;source&#39;)&lt;/script&gt;"));
        assert!(!source.contains("<script>alert('source')</script>"));

        let text = listing_page(directory.path(), &format!("/blob/{commit_id}/README.txt"));
        assert_eq!(text.status, 200);
        assert!(
            String::from_utf8(text.body)
                .unwrap()
                .contains("committed text")
        );

        let binary = listing_page(directory.path(), &format!("/blob/{commit_id}/src/data.bin"));
        assert_eq!(binary.status, 200);
        assert!(
            String::from_utf8(binary.body)
                .unwrap()
                .contains("Binary content (2 bytes).")
        );
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
        std::fs::write(right.path().join("local.txt"), "only in right").expect("right CWD change");
        let left_report = String::from_utf8(
            clone_report(
                left.path(),
                RuntimeIdentity {
                    database_id: [1; 16],
                    repository_id: [2; 16],
                },
            )
            .expect("left report"),
        )
        .expect("left JSON");
        let right_report = String::from_utf8(
            clone_report(
                right.path(),
                RuntimeIdentity {
                    database_id: [3; 16],
                    repository_id: [4; 16],
                },
            )
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
        client
            .write_all(b"GET /orna/live/")
            .expect("first fragment");
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
        for (index, branch, dirty) in [(0_u8, "http-left", false), (1_u8, "http-right", true)] {
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
            assert_eq!(
                String::from_utf8(cloned_head).unwrap().trim(),
                expected_head
            );
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
            assert_eq!(
                String::from_utf8(published_head).unwrap().trim(),
                expected_head
            );

            let mut client = TcpStream::connect(address).expect("clone report connection");
            client
                .write_all(format!("GET /api/clone HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes())
                .expect("clone report request");
            let mut response = Vec::new();
            client
                .read_to_end(&mut response)
                .expect("clone report response");
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
