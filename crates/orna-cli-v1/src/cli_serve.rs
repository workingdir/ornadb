//! The executable edge for `orna serve`.
//!
//! The host keeps Git-backed pages server-rendered and exposes authenticated
//! runtime evaluation through the live presentation transport.
//!
//! Every accepted connection runs on its own thread. Live session state is
//! shared behind one lock that is held for a single transport step at a time:
//! one HTTP session request, one WebSocket handshake phase, or the admission
//! or completion of one frame. It is never held across socket I/O or
//! evaluation, so an open WebSocket cannot stall other sockets or session
//! requests. Ending a session is the exception: DELETE and the expiry sweep
//! cancel the session's application work and keep the lock until that work
//! has stopped, because the transport drains a session in one step. The
//! session's socket workers are joined after the lock is released.

#[path = "cli_serve_app.rs"]
mod cli_serve_app;

use super::*;
use orna_application_v1::ApplicationLiveAdapter;
use orna_live_v1::{
    LiveHost, LiveSessionAuthority, LiveTransport, SessionMetadata, SystemCredentialIssuer,
    TransportLimits, WebSocketApplicationPreparation, WebSocketOutput, WebSocketState,
    WebSocketUpgrade, WireRequest, WireResponse, encode_websocket_output,
};
use orna_protocol_v1::{Envelope, Limits as ProtocolLimits, Message, PresentationContext};
use orna_repository_v1::{
    CommittedTreeEntryKind, GitCommitRef, ManagedPath, Repository, RuntimeGeneration,
};
use orna_security_v1::{
    MAX_SESSION_LEASE, Origin, OriginPolicy, SessionBoundary, SessionDeletionAdapter,
};
use orna_serving_v1::Serving;
use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration as StdDuration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_GIT_REQUEST_BODY_BYTES: usize = 512 * 1024 * 1024;
const SESSION_LEASE_MS: u64 = MAX_SESSION_LEASE;
/// Wall-clock bound for one request line and its headers. A peer that
/// trickles bytes cannot hold a connection thread past it.
const REQUEST_HEAD_TIMEOUT: StdDuration = StdDuration::from_secs(15);
const WEBSOCKET_READ_CHUNK_BYTES: usize = 8 * 1024;
/// RFC 6455 close code sent to a peer that stayed silent past the keepalive
/// bound (1001, going away).
const CLOSE_IDLE: u16 = 1001;
/// RFC 6455 close code sent when the session retired this socket's
/// attachment, for example because a resume replaced it (1008, policy
/// violation).
const CLOSE_RETIRED: u16 = 1008;
/// Interval at which the owner sweep looks for sessions whose lease has
/// elapsed.
const SESSION_SWEEP_INTERVAL: StdDuration = StdDuration::from_secs(1);
/// Bound on how long DELETE waits for the socket workers it cancelled to
/// exit before it reports orderly termination. A cancelled worker finishes
/// its in-flight step, sends a Close frame and exits.
const SESSION_TERMINATION_TIMEOUT: StdDuration = StdDuration::from_secs(60);
/// Longest the accept loop waits for a refused connection's request head.
const REFUSAL_HEAD_TIMEOUT: StdDuration = StdDuration::from_millis(250);
/// Default for [`ServeOptions::max_connections`].
const MAX_CONNECTIONS: usize = 64;

/// Operator choices for one `orna serve` process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ServeOptions {
    /// Smart-HTTP push (`git-receive-pack`) changes refs in the served clone,
    /// so it is refused unless the operator passes `serve --allow-push`.
    allow_push: bool,
    keepalive: WebSocketKeepalive,
    /// Lifetime of a live session, which is also its reconnection lease. The
    /// owner sweep terminates a session once it has elapsed.
    session_lease_ms: u64,
    /// Bound on concurrent connections, each served by its own thread. The
    /// accept loop answers a connection beyond it with 503 without reading
    /// its request.
    max_connections: usize,
    /// Bound on receiving one HTTP request body and on sending one response.
    transfer: TransferBudget,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            allow_push: false,
            keepalive: WebSocketKeepalive::default(),
            session_lease_ms: SESSION_LEASE_MS,
            max_connections: MAX_CONNECTIONS,
            transfer: TransferBudget::default(),
        }
    }
}

/// Wall-clock budget for one HTTP transfer, a request body or a response: a
/// fixed allowance plus the time the transfer takes at a minimum rate. It
/// covers the whole transfer, so a peer that trickles a body, or reads a
/// response a few bytes at a time, cannot hold a connection thread past it,
/// while a 512 MiB Git push at the minimum rate still fits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TransferBudget {
    /// Allowance for any transfer, however small.
    base: StdDuration,
    /// The slowest transfer rate the budget allows for, in bytes per second.
    min_bytes_per_second: u64,
}

impl Default for TransferBudget {
    fn default() -> Self {
        Self {
            base: StdDuration::from_secs(15),
            min_bytes_per_second: 64 * 1024,
        }
    }
}

impl TransferBudget {
    /// The deadline for moving `bytes`, starting now.
    fn deadline(self, bytes: usize) -> Instant {
        let bytes = u128::from(u64::try_from(bytes).unwrap_or(u64::MAX));
        let millis = bytes * 1000 / u128::from(self.min_bytes_per_second.max(1));
        let allowance = self.base.saturating_add(StdDuration::from_millis(
            u64::try_from(millis).unwrap_or(u64::MAX),
        ));
        let now = Instant::now();
        // An allowance beyond the clock's range fails closed.
        now.checked_add(allowance).unwrap_or(now)
    }
}

/// Liveness bounds for one attached WebSocket.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WebSocketKeepalive {
    /// Inbound silence after which the host sends a Ping.
    ping_after: StdDuration,
    /// Inbound silence after which the peer is treated as gone. Its
    /// attachment is closed, which starts the session's reconnection lease.
    idle_timeout: StdDuration,
    /// Bound on one blocked write to a stalled peer.
    write_timeout: StdDuration,
}

impl Default for WebSocketKeepalive {
    fn default() -> Self {
        Self {
            ping_after: StdDuration::from_secs(15),
            idle_timeout: StdDuration::from_secs(30),
            write_timeout: StdDuration::from_secs(30),
        }
    }
}

/// The names under which a loopback listener is its own origin: the IPv4 and
/// IPv6 loopback literals and `localhost`, each with the bound port. Any other
/// Host is a name that the peer resolved to this listener by itself, for
/// example through DNS rebinding, and is refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LoopbackAuthority {
    port: u16,
}

impl LoopbackAuthority {
    const HOSTS: [&'static str; 3] = ["127.0.0.1", "localhost", "[::1]"];

    fn authorities(self) -> impl Iterator<Item = String> {
        Self::HOSTS.into_iter().flat_map(move |host| {
            // Host and Origin omit the scheme's default port (RFC 9110).
            let implicit_port = (self.port == 80).then(|| host.to_owned());
            std::iter::once(format!("{host}:{}", self.port)).chain(implicit_port)
        })
    }

    fn permits_host(self, host: &str) -> bool {
        self.authorities()
            .any(|authority| authority.eq_ignore_ascii_case(host))
    }

    fn origins(self) -> Result<Vec<Origin>, orna_security_v1::BoundaryError> {
        self.authorities()
            .map(|authority| Origin::parse(format!("http://{authority}")))
            .collect()
    }

    /// Refuses a request that names another authority in Host, and a
    /// state-changing request whose Origin is not the request's own origin.
    /// Browsers send Origin with every cross-origin write and with every
    /// WebSocket handshake. Git's HTTP client sends none, so Git requests
    /// without an Origin are admitted while other writes must carry one.
    fn refusal(self, request: &Request) -> Option<Response> {
        let host = request_header(request, "host").unwrap_or_default();
        if !self.permits_host(host) {
            return Some(forbidden_response(request, "host_denied"));
        }
        if request.method == "GET" && !is_websocket_route(request) {
            return None;
        }
        let admitted = match request_header(request, "origin") {
            Some(origin) => is_same_origin(origin, host),
            None => request.path.starts_with("/git/"),
        };
        (!admitted).then(|| forbidden_response(request, "origin_denied"))
    }
}

fn is_same_origin(origin: &str, host: &str) -> bool {
    match (
        Origin::parse(origin),
        Origin::parse(format!("http://{host}")),
    ) {
        (Ok(origin), Ok(own)) => origin == own,
        _ => false,
    }
}

struct ServeState {
    root: PathBuf,
    identity: RuntimeIdentity,
    loopback: LoopbackAuthority,
    options: ServeOptions,
    transport_limits: TransportLimits,
    /// The live transport and its HTTP session adapters, locked for one
    /// transport step at a time (see [`ServeState::with_sessions`]).
    sessions: Mutex<SessionState>,
    /// The evaluator. It has its own lock so evaluation never blocks session
    /// requests or other sockets' frame admission.
    application: Mutex<ApplicationLiveAdapter>,
    /// The admitted `serve` entry that answers `/`; `None` when the project
    /// declares no serving entry.
    app: Option<Mutex<cli_serve_app::ServeApp>>,
    workers: WebSocketWorkers,
    /// Connections being served, bounded by [`ServeOptions::max_connections`]
    /// (see [`ConnectionSlot`]).
    connections: AtomicUsize,
}

struct SessionState {
    live: LiveTransport,
    authority: LocalSessionAuthority,
    issuer: SystemCredentialIssuer,
    deletion: LocalSessionDeletion,
}

impl ServeState {
    /// Runs one transport step under the session lock, then hands each
    /// attachment the transport retired (by resume, replacement, close,
    /// deletion, expiry or an abandoned handshake) to the socket worker that
    /// owns it for cancellation. A retirement whose worker has already exited
    /// is acknowledged here. A running worker acknowledges its own retirement
    /// as its last step, so the transport keeps the session fenced until no
    /// worker can act for the retired attachment.
    fn with_sessions<T>(&self, step: impl FnOnce(&mut SessionState) -> T) -> io::Result<T> {
        self.with_sessions_retiring(step).map(|(result, _)| result)
    }

    /// [`Self::with_sessions`], also returning the attachments whose running
    /// workers the step cancelled, so that the caller can wait for them to
    /// exit (see [`WebSocketWorkers::join`]).
    fn with_sessions_retiring<T>(
        &self,
        step: impl FnOnce(&mut SessionState) -> T,
    ) -> io::Result<(T, Vec<[u8; 16]>)> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| io::Error::other("live session state is unavailable"))?;
        let result = step(&mut sessions);
        let mut cancelled = Vec::new();
        for attachment in sessions.live.take_retired_attachments() {
            if self.workers.cancel(attachment) {
                cancelled.push(attachment);
            } else {
                sessions.live.acknowledge_retired_attachment(attachment);
            }
        }
        Ok((result, cancelled))
    }
}

/// Socket workers that own attached or attaching WebSockets, keyed by
/// attachment identity, so the transport's retirements reach them.
#[derive(Default)]
struct WebSocketWorkers {
    workers: Mutex<BTreeMap<[u8; 16], RegisteredWorker>>,
    /// Signalled each time a worker exits, for requests that wait for the
    /// workers they cancelled.
    exited: Condvar,
}

struct RegisteredWorker {
    /// A second handle on the worker's socket. Shutting down its read half
    /// wakes a worker that is blocked reading from the peer.
    socket: TcpStream,
    cancelled: Arc<AtomicBool>,
    /// The transport handed this attachment over for retirement while the
    /// worker was running; the worker acknowledges it when it exits.
    retired: bool,
}

impl WebSocketWorkers {
    fn register(&self, attachment: [u8; 16], socket: &TcpStream) -> io::Result<Arc<AtomicBool>> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker = RegisteredWorker {
            socket: socket.try_clone()?,
            cancelled: Arc::clone(&cancelled),
            retired: false,
        };
        let mut workers = self.lock();
        if workers.contains_key(&attachment) {
            return Err(io::Error::other("socket identity is already in use"));
        }
        workers.insert(attachment, worker);
        Ok(cancelled)
    }

    /// Cancels the worker that owns a retired attachment: it stops reading
    /// new frames, finishes the work it has in flight (a resume replaces the
    /// attachment, not its owned tasks) and exits. Returns `false` when no
    /// worker owns the attachment any more, so the caller acknowledges the
    /// retirement itself.
    fn cancel(&self, attachment: [u8; 16]) -> bool {
        let mut workers = self.lock();
        let Some(worker) = workers.get_mut(&attachment) else {
            return false;
        };
        worker.retired = true;
        worker.cancelled.store(true, Ordering::Release);
        let _ = worker.socket.shutdown(Shutdown::Read);
        true
    }

    /// Removes an exiting worker and reports whether it still owes the
    /// transport an acknowledgement for its retirement.
    fn finish(&self, attachment: [u8; 16]) -> bool {
        let retired = self
            .lock()
            .remove(&attachment)
            .is_some_and(|worker| worker.retired);
        self.exited.notify_all();
        retired
    }

    /// Waits until no worker owns any of `attachments`, or until `deadline`.
    /// Returns whether they have all exited.
    fn join(&self, attachments: &[[u8; 16]], deadline: Instant) -> bool {
        let mut workers = self.lock();
        loop {
            if !attachments
                .iter()
                .any(|attachment| workers.contains_key(attachment))
            {
                return true;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            workers = self
                .exited
                .wait_timeout(workers, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<[u8; 16], RegisteredWorker>> {
        // Each critical section is one map operation, so a panic elsewhere
        // cannot leave the registry half-updated.
        self.workers.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct LocalSessionAuthority {
    identity: RuntimeIdentity,
    lease_ms: u64,
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
            expires_at: now.saturating_add(self.lease_ms),
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

pub(super) fn run(endpoint: &Endpoint, port: u16, allow_push: bool) -> Result<(), Diagnostic> {
    let clone = open_served_clone(endpoint)?;
    let listener = LiveTransport::bind_default_listener(port).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "the loopback serving listener could not be bound",
            "choose a free port with `serve --port PORT`, then retry",
        )
    })?;
    let address = listener.status().address;
    let state = new_serve_state(
        clone,
        address,
        ServeOptions {
            allow_push,
            ..ServeOptions::default()
        },
    )?;
    writeln!(
        io::stdout().lock(),
        "Serving {} at http://{} (loopback{})",
        state.root.display(),
        address,
        if allow_push { "; Git push enabled" } else { "" }
    )
    .map_err(|_| {
        Diagnostic::target(
            "E2200",
            "serving status could not be written",
            "check the output stream, then start `serve` again",
        )
    })?;

    serve_listener(listener.listener(), &Arc::new(state)).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "the serving listener stopped",
            "check local socket access, then retry `serve`",
        )
    })
}

/// The clone one `orna serve` process hosts.
struct ServedClone {
    root: PathBuf,
    identity: RuntimeIdentity,
    catalogue: orna_semantic_v1::Catalogue,
    project: Option<orna_project_v1::LoadedProject>,
    /// The admitted `serve` entry, or `None` when the project declares none.
    app: Option<cli_serve_app::ServeApp>,
    /// The clone's durable runtime. The live host owns it, so each live
    /// request is durably admitted and an Eval's writes commit together with
    /// its terminal outcome (30-protocol REQUEST-1).
    runtime: RuntimeState,
    /// The factory for the served project's table-assertion validator, or
    /// `None` when the project declares no assertion. The factory runs once per
    /// commit against that commit's generation, so a false assertion refuses the
    /// write before any row is published (ORNA-ASSERT-042).
    table_candidate_validator: Option<orna_live_v1::TableCandidateValidatorFactory>,
}

/// Resolves the clone at `endpoint` before the listener binds.
fn open_served_clone(endpoint: &Endpoint) -> Result<ServedClone, Diagnostic> {
    let root = local_project_path(endpoint)?;
    let repository = Repository::discover(root).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "local Git worktree could not be discovered",
            "run `serve` inside an Orna Git worktree or provide a local project path",
        )
    })?;
    // Project loading parses the reachable source modules only. In
    // particular, serving a clone never invokes its root `main()`; functions
    // run only after an explicit query or live Eval request reaches a route.
    let (project, catalogue, app) = admit_serving_entry(endpoint);
    let (identity, initial_digest) = runtime_identity(&repository)?;
    // The served runtime uses this process's owner. A later process takes the
    // lease over with a bumped epoch once it holds the runtime owner lock, so a
    // killed predecessor's token is fenced. Test
    // `served_runtime_holds_the_process_owner_lease` pins the holder's lease.
    let owner_id = invocation_owner_id();
    let executor = current_thread_executor().map_err(|_| {
        Diagnostic::target(
            "E2200",
            "the clone's runtime state could not be opened",
            "check the local runtime state, then retry `serve`",
        )
    })?;
    let runtime = executor
        .block_on(RuntimeState::open_with_owner_id(
            &repository,
            identity,
            initial_digest,
            owner_id,
        ))
        .map_err(|error| {
            Diagnostic::target_with_detail(
                "E2200",
                "the clone's runtime state could not be opened",
                "check the local runtime state, then retry `serve`",
                format!("runtime error: {error:?}"),
            )
        })?;
    // A project that declares assertions checks each committed mounted-row
    // write against them (ORNA-ASSERT-042). A project without assertions keeps
    // the unvalidated activation path, as the REPL commit does.
    let table_candidate_validator = project
        .as_ref()
        .filter(|project| orna_conformance_v1::project_declares_assertions(project))
        .map(|project| {
            served_table_candidate_validator_factory(repository.clone(), project.clone())
        });
    Ok(ServedClone {
        root: repository.worktree().to_path_buf(),
        identity,
        catalogue,
        project,
        app,
        runtime,
        table_candidate_validator,
    })
}

/// Builds the factory that checks each committed mounted-row write against
/// the served project's table assertions. The factory runs once per commit,
/// against that commit's own generation, so the validator and the row it
/// checks share one HEAD.
fn served_table_candidate_validator_factory(
    repository: Repository,
    project: orna_project_v1::LoadedProject,
) -> orna_live_v1::TableCandidateValidatorFactory {
    Box::new(move |context: &orna_runtime_v1::RuntimeActivationContext| {
        served_table_candidate_validator(&repository, &project, context)
    })
}

/// Builds the table-assertion validator for one staged commit. Any failure to
/// read the generation, its committed identities or the admitted project
/// fails the commit closed as `RuntimeUnavailable`, so the write is refused
/// before any row is published.
fn served_table_candidate_validator(
    repository: &Repository,
    project: &orna_project_v1::LoadedProject,
    context: &orna_runtime_v1::RuntimeActivationContext,
) -> orna_live_v1::Result<Box<dyn orna_runtime_v1::TableActivationCandidateValidator>> {
    let generation = u64::try_from(context.capture().generation())
        .map_err(|_| orna_live_v1::Error::RuntimeUnavailable)?;
    let cwd_generation = repository
        .cwd_generation(RuntimeGeneration::new(generation))
        .map_err(|_| orna_live_v1::Error::RuntimeUnavailable)?;
    let commit = cwd_generation
        .head()
        .ok_or(orna_live_v1::Error::RuntimeUnavailable)?;
    let identities =
        orna_project_v1::ObjectIdentityManifest::read_committed_if_present(repository, commit)
            .map_err(|_| orna_live_v1::Error::RuntimeUnavailable)?
            .ok_or(orna_live_v1::Error::RuntimeUnavailable)?;
    let table_object_ids = served_committed_table_object_ids(&identities)?;
    orna_conformance_v1::transactional_loaded_project_candidate_validator(
        project,
        &identities,
        Some(cwd_generation),
        table_object_ids,
        Limits::default(),
    )
    .map_err(|_| orna_live_v1::Error::RuntimeUnavailable)
}

/// The committed identity of every live table, keyed by qualified name. Two
/// live identities under one name are ambiguous, so they fail the commit
/// rather than picking one. The REPL's `committed_table_identities` checks
/// only the names it binds; this host binds every live table, so a duplicate
/// under any name fails the commit.
fn served_committed_table_object_ids(
    identities: &orna_project_v1::ObjectIdentityManifest,
) -> orna_live_v1::Result<Arc<BTreeMap<String, orna_runtime_v1::TableObjectId>>> {
    let mut table_object_ids = BTreeMap::new();
    for record in identities.records() {
        if record.kind() != orna_project_v1::ObjectKind::Table || record.is_retired() {
            continue;
        }
        let identity = orna_runtime_v1::TableObjectId::new(*record.id());
        if table_object_ids
            .insert(record.qualified_name().to_owned(), identity)
            .is_some()
        {
            return Err(orna_live_v1::Error::RuntimeUnavailable);
        }
    }
    Ok(Arc::new(table_object_ids))
}

/// Loads and admits the serving entry. A broken `main.orna` is reported on
/// stderr and leaves the Git transport serving (ORNA-SERVE-007, ORNA-APP-020):
/// the project and entry are then absent and the catalogue is empty.
fn admit_serving_entry(
    endpoint: &Endpoint,
) -> (
    Option<orna_project_v1::LoadedProject>,
    orna_semantic_v1::Catalogue,
    Option<cli_serve_app::ServeApp>,
) {
    let project = match load_project(endpoint) {
        Ok(project) => project,
        Err(diagnostic) => {
            report_serving_diagnostic(&diagnostic);
            return (None, orna_semantic_v1::Catalogue::default(), None);
        }
    };
    let catalogue = match semantic_catalogue(&project) {
        Ok(catalogue) => catalogue,
        Err(diagnostic) => {
            report_serving_diagnostic(&diagnostic);
            return (None, orna_semantic_v1::Catalogue::default(), None);
        }
    };
    let root = match local_project_path(endpoint) {
        Ok(root) => root,
        Err(diagnostic) => {
            report_serving_diagnostic(&diagnostic);
            return (Some(project), catalogue, None);
        }
    };
    match cli_serve_app::ServeApp::admit(root, &project, &catalogue) {
        Ok(app) => {
            if app.is_none() {
                // Without a serving entry the Git transport still serves; `/` is absent.
                report_serving_diagnostic(&cli_serve_app::missing_entry_diagnostic());
            }
            (Some(project), catalogue, app)
        }
        Err(diagnostic) => {
            report_serving_diagnostic(&diagnostic);
            (Some(project), catalogue, None)
        }
    }
}

fn report_serving_diagnostic(diagnostic: &Diagnostic) {
    let _ = write_diagnostic(&mut io::stderr().lock(), diagnostic, false, false);
}

fn new_serve_state(
    clone: ServedClone,
    listener_address: SocketAddr,
    options: ServeOptions,
) -> Result<ServeState, Diagnostic> {
    let ServedClone {
        root,
        identity,
        catalogue,
        project,
        app,
        runtime,
        table_candidate_validator,
    } = clone;
    let loopback = LoopbackAuthority {
        port: listener_address.port(),
    };
    let origins = loopback.origins().map_err(|_| {
        Diagnostic::target(
            "E2100",
            "the loopback serving origin is invalid",
            "retry `serve` with a supported local listener",
        )
    })?;
    let boundary = SessionBoundary::new(OriginPolicy::new(origins, []), options.session_lease_ms);
    let host = LiveHost::with_runtime_state_and_owner(
        orna_live_v1::Limits::default(),
        boundary,
        Serving::new(orna_serving_v1::Limits::default()).map_err(|_| {
            Diagnostic::target(
                "E2100",
                "serving limits could not be initialized",
                "retry `serve`; the local serving state is unavailable",
            )
        })?,
        runtime,
        // The served runtime was opened with this same process owner (see
        // `open_served_clone`), so the live host's `acquire_lease` re-acquires it.
        invocation_owner_id(),
    )
    .map_err(|_| {
        Diagnostic::target(
            "E2100",
            "the live protocol host could not be initialized",
            "retry `serve`; the local serving state is unavailable",
        )
    })?;
    // The served project's table assertions gate every committed mounted-row
    // write. A project without assertions installs no validator.
    let host = match table_candidate_validator {
        Some(factory) => host.with_table_candidate_validator(factory),
        None => host,
    };

    // The listener is plaintext HTTP on loopback, the explicitly trusted
    // transport 30-protocol:5 permits. `Secure` is reserved for TLS
    // (30-protocol:15), so the session cookie omits it here.
    let transport_limits = TransportLimits {
        tls: false,
        ..TransportLimits::default()
    };
    let live = LiveTransport::new(host, transport_limits).map_err(|_| {
        Diagnostic::target(
            "E2100",
            "the live transport could not be initialized",
            "retry `serve`; the local serving state is unavailable",
        )
    })?;
    let std_snapshot = project
        .as_ref()
        .map_or(StdSnapshotPin::Unpinned, sys_host::std_snapshot_pin);
    let sys_host_context =
        sys_host::sys_host_context(&root, identity, RuntimeMode::Serve, std_snapshot);
    // Every Eval is one input to its session's REPL (30-protocol); the host
    // selects no application module for Eval admission.
    let mut application = ApplicationLiveAdapter::new(
        ApplicationAuthority::new(catalogue, Limits::default())
            .with_sys_host_context(sys_host_context),
    )
    .with_runtime_identity(identity.database_id, identity.repository_id);
    if let Some(project) = project {
        application = application.with_loaded_project(project);
    }
    Ok(ServeState {
        root,
        identity,
        loopback,
        options,
        transport_limits,
        sessions: Mutex::new(SessionState {
            live,
            authority: LocalSessionAuthority {
                identity,
                lease_ms: options.session_lease_ms,
            },
            issuer: SystemCredentialIssuer::default(),
            deletion: LocalSessionDeletion,
        }),
        application: Mutex::new(application),
        app: app.map(Mutex::new),
        workers: WebSocketWorkers::default(),
        connections: AtomicUsize::new(0),
    })
}

fn serve_listener(listener: &TcpListener, state: &Arc<ServeState>) -> io::Result<()> {
    start_session_sweep(state)?;
    let refusals = Arc::new(AtomicUsize::new(0));
    for accepted in listener.incoming() {
        let stream = match accepted {
            Ok(stream) => stream,
            // A peer that resets before accept, or a process that briefly runs
            // out of descriptors, must not end the listener for every client.
            Err(error) if is_transient_accept_error(&error) => {
                std::thread::sleep(ACCEPT_RETRY_DELAY);
                continue;
            }
            Err(error) => return Err(error),
        };
        let Some(slot) = ConnectionSlot::claim(state, &stream) else {
            refuse_off_loop(stream, &refusals);
            continue;
        };
        // A failed spawn drops the closure, which releases the slot and
        // shuts the connection; the listener keeps serving other clients.
        let _ = std::thread::Builder::new()
            .name("orna-serve-connection".into())
            .spawn(move || {
                let _ = serve_connection(stream, &slot.state);
                drop(slot);
            });
    }
    Ok(())
}

/// Refusals answered at once. Each waits up to [`REFUSAL_HEAD_TIMEOUT`] for
/// its request head on a thread of its own; beyond this many, the connection
/// is answered from the accept loop by [`refuse_saturated`] instead.
const MAX_REFUSALS_IN_FLIGHT: usize = 16;

/// Bound on each wait the accept loop makes while answering a saturated
/// refusal: first for the request line to arrive, then for the canned 503 to
/// be written. One saturated refusal therefore holds the accept loop for at
/// most twice this long.
const SATURATED_REFUSAL_WAIT: StdDuration = StdDuration::from_millis(5);

/// Hands a connection beyond the cap to [`refuse_connection`] on its own
/// thread, so the accept loop never waits on a silent peer.
fn refuse_off_loop(stream: TcpStream, in_flight: &Arc<AtomicUsize>) {
    let claimed = in_flight.fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
        (active < MAX_REFUSALS_IN_FLIGHT).then_some(active + 1)
    });
    if claimed.is_err() {
        refuse_saturated(stream);
        return;
    }
    let worker = Arc::clone(in_flight);
    let spawned = std::thread::Builder::new()
        .name("orna-serve-refusal".into())
        .spawn(move || {
            refuse_connection(stream);
            worker.fetch_sub(1, Ordering::AcqRel);
        });
    if spawned.is_err() {
        in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Answers a connection that arrives while every refusal thread is busy,
/// from the accept loop, without a head read. The request line is only
/// peeked, so the route shape can be chosen while the bytes stay unread, and
/// both the peek and the write are bounded by [`SATURATED_REFUSAL_WAIT`]. A
/// peer that sends nothing in time gets the generic shape.
fn refuse_saturated(mut stream: TcpStream) {
    let orna_route = peek_is_orna_route(&stream, Instant::now() + SATURATED_REFUSAL_WAIT);
    let response = if orna_route {
        live_error_response(503, "live.unavailable", "request rejected")
    } else {
        unavailable_response()
    };
    let head = response_head(&response);
    let _ = write_before(
        &mut stream,
        &[&head, &response.body],
        Instant::now() + SATURATED_REFUSAL_WAIT,
    );
    let _ = stream.shutdown(Shutdown::Write);
}

/// Peeks at the start of a connection's request line until it can classify
/// the route or `deadline` passes. Bytes are never consumed.
fn peek_is_orna_route(stream: &TcpStream, deadline: Instant) -> bool {
    // The longest `/orna/` method prefix, `DELETE /orna/`, is 13 bytes.
    const CLASSIFY_BYTES: usize = 13;
    let mut probe = [0_u8; CLASSIFY_BYTES];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || stream.set_read_timeout(Some(remaining)).is_err() {
            return false;
        }
        match stream.peek(&mut probe) {
            Ok(read) if read >= CLASSIFY_BYTES || probe[..read].contains(&b'\n') => {
                return is_orna_request_line(&probe[..read]);
            }
            // Nothing, or only part of the line so far: wait briefly for more.
            Ok(0) => return false,
            Ok(_) => std::thread::sleep(StdDuration::from_micros(200)),
            Err(_) => return false,
        }
    }
}

/// How long the accept loop pauses after a transient accept failure before
/// trying again, so a descriptor shortage does not spin the loop.
const ACCEPT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(10);

/// Whether an accept failure only concerns one attempt or a temporary
/// resource shortage, so the listener should retry rather than stop.
/// The errno values are Linux's: EMFILE 24, ENFILE 23, ENOMEM 12, ENOBUFS 105.
fn is_transient_accept_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::Interrupted
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
    ) || matches!(error.raw_os_error(), Some(12 | 23 | 24 | 105))
}

/// One accepted connection's claim on [`ServeOptions::max_connections`],
/// held by the thread that serves it. Dropping it releases the claim before
/// it ends the connection, so a peer that has seen its connection end can
/// connect again at once.
struct ConnectionSlot {
    state: Arc<ServeState>,
    /// A second handle on the connection's socket, which keeps the socket
    /// open until the claim has been released.
    socket: TcpStream,
}

impl ConnectionSlot {
    /// Claims a slot for `stream`, or returns `None` when every slot is
    /// taken.
    fn claim(state: &Arc<ServeState>, stream: &TcpStream) -> Option<Self> {
        let limit = state.options.max_connections;
        state
            .connections
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < limit).then_some(active + 1)
            })
            .ok()?;
        match stream.try_clone() {
            Ok(socket) => Some(Self {
                state: Arc::clone(state),
                socket,
            }),
            Err(_) => {
                state.connections.fetch_sub(1, Ordering::AcqRel);
                None
            }
        }
    }
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.state.connections.fetch_sub(1, Ordering::AcqRel);
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}

/// Answers a connection beyond [`ServeOptions::max_connections`] from the
/// accept loop itself, with a 503 in the shape its route expects (30-protocol,
/// Creation/resume errors: `{ "code", "message" }` on `/orna/`). The request
/// head is read first, bounded by [`REFUSAL_HEAD_TIMEOUT`], on the refusal's
/// own thread (see [`refuse_off_loop`]), so a silent peer cannot hold up the
/// accept loop.
fn refuse_connection(stream: TcpStream) {
    let mut reader = BufReader::new(stream);
    let mut orna_route = false;
    // Whatever the head read yields, the route is known from its request line
    // when that line arrived; an unreadable head gets the generic shape.
    let _ = read_request_head(
        &mut reader,
        Instant::now() + REFUSAL_HEAD_TIMEOUT,
        &mut orna_route,
    );
    let response = if orna_route {
        live_error_response(503, "live.unavailable", "request rejected")
    } else {
        unavailable_response()
    };
    let _ = write_response(reader.get_mut(), response, TransferBudget::default());
    let _ = reader.get_mut().shutdown(Shutdown::Write);
}

/// Maps a live WebSocket failure to the RFC 6455 close code the socket is
/// closed with: 1009 when a message exceeds its size bound, 1002 for any
/// other protocol violation (30-protocol close codes).
fn close_code_for_failure(error: orna_live_v1::Error) -> u16 {
    match error {
        orna_live_v1::Error::Limit => 1009,
        _ => 1002,
    }
}

/// Starts the owner sweep (30-protocol, Session creation and ownership:
/// expiry terminates the owner and cancels its children). Every
/// [`SESSION_SWEEP_INTERVAL`] it ends each session whose lease has elapsed
/// through the same child-aware drain as DELETE, together with abandoned
/// handshakes, and [`ServeState::with_sessions`] cancels their socket
/// workers. The sweep stops once the serving state has been dropped.
fn start_session_sweep(state: &Arc<ServeState>) -> io::Result<()> {
    let state = Arc::downgrade(state);
    let executor = current_thread_executor()?;
    std::thread::Builder::new()
        .name("orna-serve-session-sweep".into())
        .spawn(move || sweep_sessions(&state, &executor))
        .map(drop)
}

fn sweep_sessions(state: &Weak<ServeState>, executor: &tokio::runtime::Runtime) {
    loop {
        std::thread::sleep(SESSION_SWEEP_INTERVAL);
        let Some(state) = state.upgrade() else {
            return;
        };
        // A session whose drain fails stays in place, and the next pass
        // retries it.
        let _ = state.with_sessions(|sessions| {
            let SessionState { live, deletion, .. } = sessions;
            let mut children = live.application_work_supervisor();
            executor.block_on(live.expire_sessions_with_children(now_ms(), deletion, &mut children))
        });
    }
}

fn serve_connection(stream: TcpStream, state: &ServeState) -> io::Result<()> {
    let transfer = state.options.transfer;
    let mut reader = BufReader::new(stream);
    let mut orna_route = false;
    let Ok((mut request, body_length)) = read_request_head(
        &mut reader,
        Instant::now() + REQUEST_HEAD_TIMEOUT,
        &mut orna_route,
    ) else {
        let response = if orna_route {
            live_error_response(400, "wire.invalid_message", "malformed request")
        } else {
            bad_request_response()
        };
        return write_response(reader.get_mut(), response, transfer);
    };
    // Refuse from the head alone, before the body is read or allocated, so a
    // foreign or cross-origin request cannot make this host wait for or
    // buffer up to the route's body limit. This runs before the size check so
    // an oversize foreign request is refused as foreign, not sized.
    if let Some(refusal) = state.loopback.refusal(&request) {
        return write_response(reader.get_mut(), refusal, transfer);
    }
    if body_length > request_body_limit(&request.path) {
        let response = if request.path.starts_with("/orna/") {
            live_error_response(413, "wire.limit", "request body exceeds its limit")
        } else {
            Response::new(
                413,
                "application/json",
                br#"{"error":"payload_too_large"}"#.to_vec(),
            )
        };
        return write_response(reader.get_mut(), response, transfer);
    }
    let Ok(body) = read_request_body(&mut reader, body_length, transfer.deadline(body_length))
    else {
        return write_response(reader.get_mut(), bad_request_response(), transfer);
    };
    request.body = body;
    if is_websocket_route(&request) {
        // Bytes the peer sent after its handshake belong to the WebSocket.
        let initial = reader.buffer().to_vec();
        let stream = reader.into_inner();
        return serve_websocket(state, &stream, request, initial);
    }
    let mut stream = reader.into_inner();
    if request.path.starts_with("/orna/session") {
        return serve_session_http_request(&mut stream, request, state);
    }
    if let Some(response) = cli_serve_app::serving_entry_route(state.app.as_ref(), &request) {
        return write_response(&mut stream, response, transfer);
    }
    let response = host_route(
        &state.root,
        state.identity,
        state.options.allow_push,
        &request,
    );
    write_response(&mut stream, response, transfer)
}

/// Answers one `/orna/session` request through the transport's child-aware
/// entry, so DELETE cancels and joins the session's application work
/// through the work supervisor. DELETE answers 204 only once the socket
/// workers it cancelled have exited too (30-protocol, Session creation and
/// ownership: 204 after orderly termination).
fn serve_session_http_request(
    stream: &mut TcpStream,
    request: Request,
    state: &ServeState,
) -> io::Result<()> {
    let wire = WireRequest {
        method: request.method,
        path: request.path,
        headers: request.headers,
        body: request.body,
    };
    let executor = current_thread_executor()?;
    let (mut response, cancelled) = state.with_sessions_retiring(|sessions| {
        let SessionState {
            live,
            authority,
            issuer,
            deletion,
        } = sessions;
        let mut children = live.application_work_supervisor();
        executor.block_on(live.handle_with_children(
            wire,
            now_ms(),
            authority,
            issuer,
            deletion,
            &mut children,
        ))
    })?;
    if response.status == 204
        && !state
            .workers
            .join(&cancelled, Instant::now() + SESSION_TERMINATION_TIMEOUT)
    {
        // The session is gone, but a socket worker of it is still running,
        // so termination is not yet orderly.
        response = WireResponse {
            status: 503,
            headers: vec![("content-type".into(), "application/json".into())],
            body: br#"{"code":"live.unavailable","message":"request rejected"}"#.to_vec(),
        };
    }
    let encoded = encode_http_response(&response, state.transport_limits)?;
    write_before(
        stream,
        &[&encoded],
        state.options.transfer.deadline(encoded.len()),
    )
}

fn current_thread_executor() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|_| io::Error::other("live request executor unavailable"))
}

fn encode_http_response(response: &WireResponse, limits: TransportLimits) -> io::Result<Vec<u8>> {
    response
        .encode_http(limits)
        .map_err(|_| io::Error::other("live response could not be encoded"))
}

fn is_websocket_route(request: &Request) -> bool {
    request.method == "GET" && request.path.starts_with("/orna/live/")
}

fn serve_websocket(
    state: &ServeState,
    stream: &TcpStream,
    request: Request,
    initial: Vec<u8>,
) -> io::Result<()> {
    let executor = current_thread_executor()?;
    let mut attachment = [0; 16];
    getrandom::fill(&mut attachment)
        .map_err(|_| io::Error::other("socket identity unavailable"))?;
    stream.set_write_timeout(Some(state.options.keepalive.write_timeout))?;
    let worker = WebSocketWorker {
        state,
        executor: &executor,
        stream,
        attachment,
        cancelled: state.workers.register(attachment, stream)?,
    };
    let request = WireRequest {
        method: request.method,
        path: request.path,
        headers: request.headers,
        body: request.body,
    };
    if worker.attach(&request)? {
        worker.serve_frames(initial)?;
    }
    Ok(())
}

/// The worker thread for one WebSocket. It drives the transport's actor path
/// one step at a time: a frame is admitted under the session lock,
/// application work runs under the application lock only, and output is
/// written with neither lock held. Dropping the worker ends its connection,
/// unregisters it, closes its attachment (which starts the session's
/// reconnection lease) and acknowledges a retirement the transport handed
/// it while it ran.
struct WebSocketWorker<'a> {
    state: &'a ServeState,
    executor: &'a tokio::runtime::Runtime,
    stream: &'a TcpStream,
    attachment: [u8; 16],
    cancelled: Arc<AtomicBool>,
}

enum Handshake {
    Refused(Vec<u8>),
    Deliver(WebSocketUpgrade, Vec<u8>),
}

impl WebSocketWorker<'_> {
    /// Completes the RFC 6455 handshake and commits the attachment. Returns
    /// `false` once a refusal has been written.
    fn attach(&self, request: &WireRequest) -> io::Result<bool> {
        let limits = self.state.transport_limits;
        let now = now_ms();
        let handshake = self.state.with_sessions(|sessions| {
            let live = &mut sessions.live;
            let upgrade = match live.begin_websocket_upgrade(request, self.attachment, now) {
                Ok(upgrade) => upgrade,
                Err(refusal) => {
                    return encode_http_response(&refusal, limits).map(Handshake::Refused);
                }
            };
            // The reservation stays held while the 101 is written, so resume,
            // deletion and expiry cannot consume it before the commit.
            match encode_http_response(upgrade.response(), limits) {
                Ok(response)
                    if upgrade.response().status == 101
                        && live.begin_websocket_upgrade_delivery(&upgrade, now) =>
                {
                    Ok(Handshake::Deliver(upgrade, response))
                }
                _ => {
                    live.abort_websocket_upgrade(&upgrade);
                    Err(io::Error::other(
                        "the WebSocket handshake could not be delivered",
                    ))
                }
            }
        })??;
        let mut stream = self.stream;
        let (upgrade, response) = match handshake {
            Handshake::Refused(refusal) => {
                stream.write_all(&refusal)?;
                stream.flush()?;
                return Ok(false);
            }
            Handshake::Deliver(upgrade, response) => (upgrade, response),
        };
        if let Err(error) = stream.write_all(&response).and_then(|()| stream.flush()) {
            self.state
                .with_sessions(|sessions| sessions.live.abort_websocket_upgrade(&upgrade))?;
            return Err(error);
        }
        self.state.with_sessions(|sessions| {
            let live = &mut sessions.live;
            if !live.finish_websocket_upgrade_delivery(&upgrade) {
                live.abort_websocket_upgrade(&upgrade);
                return Err(io::Error::other(
                    "the WebSocket handoff was retired before its commit",
                ));
            }
            self.executor
                .block_on(live.commit_websocket_upgrade(upgrade, now_ms()))
                .map(drop)
                .map_err(|_| io::Error::other("the WebSocket attachment was refused"))
        })??;
        Ok(true)
    }

    /// Serves frames until the peer closes, the transport retires this
    /// attachment, or the peer stays silent past the keepalive bound. The
    /// last two end with a best-effort Close frame.
    /// Silence is measured from the peer's last bytes or the end of the last
    /// step served for it, whichever is later: time spent evaluating its own
    /// requests, while nothing is read from it, is not silence.
    fn serve_frames(&self, initial: Vec<u8>) -> io::Result<()> {
        let keepalive = self.state.options.keepalive;
        let mut socket = WebSocketState::new(self.attachment);
        let mut stream = self.stream;
        let mut chunk = [0; WEBSOCKET_READ_CHUNK_BYTES];
        let mut input = initial;
        let mut last_activity = Instant::now();
        let mut pinged = false;
        loop {
            if !input.is_empty() {
                let bytes = std::mem::take(&mut input);
                if !self.serve_input(&mut socket, &bytes)? {
                    return Ok(());
                }
                last_activity = Instant::now();
            }
            if self.cancelled.load(Ordering::Acquire) {
                self.close(CLOSE_RETIRED);
                return Ok(());
            }
            let silent = last_activity.elapsed();
            if silent >= keepalive.idle_timeout {
                self.close(CLOSE_IDLE);
                return Ok(());
            }
            if !pinged && silent >= keepalive.ping_after {
                self.write(&WebSocketOutput::Ping(Vec::new()))?;
                pinged = true;
            }
            let wake = if pinged {
                keepalive.idle_timeout
            } else {
                keepalive.ping_after
            };
            stream.set_read_timeout(Some(
                wake.saturating_sub(silent).max(StdDuration::from_millis(1)),
            ))?;
            match stream.read(&mut chunk) {
                // Cancellation shuts down this socket's read half, which also
                // reads as end of stream; the loop then sends its Close frame.
                Ok(0) if self.cancelled.load(Ordering::Acquire) => {}
                Ok(0) => return Ok(()),
                Ok(read) => {
                    input.extend_from_slice(&chunk[..read]);
                    last_activity = Instant::now();
                    pinged = false;
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// Sends a Close frame before the worker drops its socket. It is bounded
    /// by the socket's write timeout, and a failed write is ignored because
    /// the connection is ending anyway.
    fn close(&self, code: u16) {
        let _ = self.write(&WebSocketOutput::Close {
            code: Some(code),
            reason: Vec::new(),
        });
    }

    /// Admits each complete frame in `bytes` and any frames already buffered
    /// behind it, one transport step per frame, and writes each output.
    /// Returns `false` once the socket has closed.
    fn serve_input(&self, socket: &mut WebSocketState, bytes: &[u8]) -> io::Result<bool> {
        let limits = self.state.transport_limits;
        let mut bytes = bytes;
        loop {
            let prepared = self.state.with_sessions(|sessions| {
                self.executor
                    .block_on(
                        sessions
                            .live
                            .prepare_websocket_application(socket, now_ms(), bytes),
                    )
            })?;
            bytes = &[];
            let output = match prepared {
                Ok(WebSocketApplicationPreparation::Pending) => None,
                Ok(WebSocketApplicationPreparation::Output(output)) => Some(output),
                Ok(WebSocketApplicationPreparation::Work(ticket)) => {
                    // Evaluation holds only the application lock, so other
                    // sockets and session requests proceed meanwhile.
                    let completion = match self.state.application.lock() {
                        Ok(mut application) => {
                            self.executor.block_on(ticket.execute(&mut *application))
                        }
                        Err(_) => ticket.reject(orna_live_v1::Error::RuntimeUnavailable),
                    };
                    let completed = self.state.with_sessions(|sessions| {
                        self.executor
                            .block_on(sessions.live.complete_application(completion))
                    })?;
                    match completed {
                        Ok(output) => Some(output),
                        Err(error) => {
                            self.close(close_code_for_failure(error));
                            return Ok(false);
                        }
                    }
                }
                // A framing or attachment failure closes this socket with the
                // code the WebSocket protocol assigns to it; dropping the
                // worker then releases its attachment.
                Err(error) => {
                    self.close(close_code_for_failure(error));
                    return Ok(false);
                }
            };
            if let Some(output) = output {
                self.write(&output)?;
                if matches!(output, WebSocketOutput::Close { .. }) {
                    return Ok(false);
                }
            }
            let buffered = socket
                .has_complete_frame(limits.max_frame_bytes)
                .map_err(|_| io::Error::other("buffered WebSocket input is unavailable"))?;
            if !buffered {
                return Ok(true);
            }
        }
    }

    fn write(&self, output: &WebSocketOutput) -> io::Result<()> {
        let frame = encode_websocket_output(output, self.state.transport_limits)
            .map_err(|_| io::Error::other("WebSocket output exceeds the outgoing bound"))?;
        if let Some(frame) = frame {
            let mut stream = self.stream;
            stream.write_all(&frame)?;
            stream.flush()?;
        }
        Ok(())
    }
}

impl Drop for WebSocketWorker<'_> {
    fn drop(&mut self) {
        // The peer sees the connection end before anyone waiting for this
        // worker to exit, such as a DELETE of its session, is released.
        let _ = self.stream.shutdown(Shutdown::Both);
        let retired = self.state.workers.finish(self.attachment);
        let _ = self.state.with_sessions(|sessions| {
            // A retired, refused or never-committed attachment is already
            // closed; the transport answers `Closed`, which needs no action.
            let _ = self
                .executor
                .block_on(sessions.live.close_attachment(self.attachment, now_ms()));
            if retired {
                sessions
                    .live
                    .acknowledge_retired_attachment(self.attachment);
            }
        });
    }
}

// The project root is selected once by `orna serve`; requests never supply a
// repository path. Browsing is based on immutable Git commits and trees.
fn host_route(
    root: &Path,
    identity: RuntimeIdentity,
    allow_push: bool,
    request: &Request,
) -> Response {
    if let Some(response) = git_transport_route(root, allow_push, request) {
        return response;
    }
    if let Some(response) = git_listing_route(root, request) {
        return response;
    }
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/api/clone") => match clone_report(root, identity) {
            Ok(report) => Response::new(200, "application/json", report),
            Err(_) => unavailable_response(),
        },
        _ => not_found_response(),
    }
}

const MAX_LISTING_TREE_ENTRIES: usize = 10_000;
const MAX_LISTING_FILE_BYTES: usize = 2 * 1024 * 1024;
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

fn git_listing_route(root: &Path, request: &Request) -> Option<Response> {
    if request.method != "GET" {
        return None;
    }
    match request.path.as_str() {
        path if path.starts_with("/tree/") => Some(tree_page(root, &path[6..])),
        path if path.starts_with("/blob/") => Some(blob_page(root, &path[6..])),
        _ => None,
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

fn git_transport_route(root: &Path, allow_push: bool, request: &Request) -> Option<Response> {
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
    // Push changes refs in the served clone. Refusing it here, rather than
    // through Git configuration, keeps the operator's `serve --allow-push`
    // choice authoritative over any `http.receivepack` setting.
    if service == "git-receive-pack" && !allow_push {
        return Some(Response::new(
            403,
            "application/json",
            br#"{"error":"push_disabled"}"#.to_vec(),
        ));
    }
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
    clear_cgi_environment(&mut command);
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
    // Only reached after the operator enabled push with `serve --allow-push`.
    // Git's ordinary receive-pack checks (including checked-out-branch
    // protection) still apply.
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

/// The CGI child inherits only this allow-list: PATH to run git, HOME and
/// TMPDIR for git's own state, and locale. Server tokens, proxy settings and
/// ambient GIT_* routing or config variables never reach it. Git locates its
/// exec path itself (compiled-in or RUNTIME_PREFIX), so GIT_EXEC_PATH is not
/// forwarded; XDG_CONFIG_HOME is not forwarded, so only ~/.gitconfig applies.
/// Requires git >= 2.30 (`commit_optional` relies on `rev-parse --end-of-options`).
fn clear_cgi_environment(command: &mut Command) {
    command.env_clear();
    for name in ["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
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

/// Reads one request line and its headers, which together must arrive
/// within `deadline` and [`MAX_HEADER_BYTES`]. Every route, including the
/// WebSocket upgrade, is chosen from this one parse. Returns the request with
/// an empty body and the declared body length; the caller checks that length
/// against the route's body limit. `orna_route` is set as soon as the request
/// line is read, so a malformed head still answers in the live error shape.
fn read_request_head(
    reader: &mut BufReader<TcpStream>,
    deadline: Instant,
    orna_route: &mut bool,
) -> io::Result<(Request, usize)> {
    let mut budget = MAX_HEADER_BYTES;
    let mut line = Vec::new();
    read_head_line(reader, &mut line, &mut budget, deadline)?;
    *orna_route = is_orna_request_line(&line);
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
        read_head_line(reader, &mut line, &mut budget, deadline)?;
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
    Ok((
        Request {
            method,
            path: path.to_owned(),
            query: query.to_owned(),
            headers,
            body: Vec::new(),
        },
        body_length,
    ))
}

/// Reads one `\n`-terminated head line into `line`, charging it to the
/// shared header `budget`. Every read is bounded by what is left before
/// `deadline`, so neither a line without a newline nor a peer that trickles
/// bytes can hold the connection past the head's byte and time bounds.
fn read_head_line(
    reader: &mut BufReader<TcpStream>,
    line: &mut Vec<u8>,
    budget: &mut usize,
    deadline: Instant,
) -> io::Result<()> {
    line.clear();
    let deadline_elapsed =
        || io::Error::new(io::ErrorKind::TimedOut, "request head deadline elapsed");
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(deadline_elapsed());
        }
        reader.get_ref().set_read_timeout(Some(remaining))?;
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(deadline_elapsed());
            }
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "incomplete request",
            ));
        }
        let (length, complete) = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or((available.len(), false), |end| (end + 1, true));
        if length > *budget {
            return Err(io::Error::other("request header limit"));
        }
        line.extend_from_slice(&available[..length]);
        reader.consume(length);
        *budget -= length;
        if complete {
            return Ok(());
        }
    }
}

/// Reads a request body of `length` bytes, which must have arrived in full
/// by `deadline`. Every read is bounded by the time left, so a peer that
/// trickles the body cannot hold the connection past the deadline.
fn read_request_body(
    reader: &mut BufReader<TcpStream>,
    length: usize,
    deadline: Instant,
) -> io::Result<Vec<u8>> {
    let mut body = vec![0; length];
    let mut filled = 0;
    while filled < length {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "request body deadline elapsed",
            ));
        }
        reader.get_ref().set_read_timeout(Some(remaining))?;
        match reader.read(&mut body[filled..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "incomplete request body",
                ));
            }
            Ok(read) => filled += read,
            // A timed-out read is judged against the deadline above.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(body)
}

/// Writes `response`, all of it within `budget`.
fn write_response(
    stream: &mut TcpStream,
    response: Response,
    budget: TransferBudget,
) -> io::Result<()> {
    let head = response_head(&response);
    let deadline = budget.deadline(head.len().saturating_add(response.body.len()));
    write_before(stream, &[&head, &response.body], deadline)
}

/// Writes `parts` in order, all of them by `deadline`. Every blocked write
/// is bounded by the time left, so a peer that stops reading, or reads a few
/// bytes at a time, cannot hold the connection past the deadline.
fn write_before(stream: &mut TcpStream, parts: &[&[u8]], deadline: Instant) -> io::Result<()> {
    for part in parts {
        let mut unsent = *part;
        while !unsent.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "response deadline elapsed",
                ));
            }
            stream.set_write_timeout(Some(remaining))?;
            match stream.write(unsent) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(written) => unsent = &unsent[written..],
                // A timed-out write is judged against the deadline above.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted
                            | io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(())
}

/// Headers on every HTML document `orna serve` produces (ORNA-APP-012,
/// ORNA-APP-013). The pages carry only inline styles and no script, so the
/// policy admits no script, object, frame, form or base target at all.
/// `frame-ancestors 'none'` and `X-Frame-Options: DENY` refuse framing by any
/// origin, including this one. `nosniff` stops MIME sniffing an HTML body as
/// another type. The referrer is withheld so an address path is not leaked to
/// a link target.
const HTML_SECURITY_HEADERS: [(&str, &str); 4] = [
    (
        "Content-Security-Policy",
        "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
    ),
    ("X-Content-Type-Options", "nosniff"),
    ("X-Frame-Options", "DENY"),
    ("Referrer-Policy", "no-referrer"),
];

/// The status line and headers of `response`, through the blank line that
/// ends them.
fn response_head(response: &Response) -> Vec<u8> {
    use std::fmt::Write as _;
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
    let mut head = format!("HTTP/1.1 {} {reason}\r\n", response.status);
    if !response.content_type.is_empty() {
        let _ = write!(head, "Content-Type: {}\r\n", response.content_type);
    }
    if response.content_type.starts_with("text/html") {
        for (name, value) in HTML_SECURITY_HEADERS {
            let _ = write!(head, "{name}: {value}\r\n");
        }
    }
    for (name, value) in &response.headers {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    let _ = write!(
        head,
        "Content-Length: {}\r\nConnection: close\r\n\r\n",
        response.body.len()
    );
    head.into_bytes()
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

/// A perimeter refusal. Live session routes answer with the protocol's
/// `{ "code", "message" }` error shape; the other routes keep this host's
/// `{ "error" }` shape.
fn forbidden_response(request: &Request, reason: &str) -> Response {
    let body = if request.path.starts_with("/orna/") {
        format!(r#"{{"code":"live.{reason}","message":"request rejected"}}"#)
    } else {
        format!(r#"{{"error":"{reason}"}}"#)
    };
    Response::new(403, "application/json", body)
}

fn bad_request_response() -> Response {
    Response::new(
        400,
        "application/json",
        br#"{"error":"bad_request"}"#.to_vec(),
    )
}

/// Whether a request line targets the live `/orna/` surface, so its errors
/// use the `{ "code", "message" }` shape (30-protocol, Creation/resume errors).
fn is_orna_request_line(line: &[u8]) -> bool {
    line.starts_with(b"GET /orna/")
        || line.starts_with(b"POST /orna/")
        || line.starts_with(b"DELETE /orna/")
}

/// The body limit for a route, which the head's declared length must not
/// exceed before the body is read.
fn request_body_limit(path: &str) -> usize {
    if path.starts_with("/git/") {
        MAX_GIT_REQUEST_BODY_BYTES
    } else {
        MAX_REQUEST_BODY_BYTES
    }
}

/// A live error in the protocol's `{ "code", "message" }` JSON. The code and
/// message are fixed constants with no characters needing JSON escapes, and
/// they carry no credentials or host paths.
fn live_error_response(status: u16, code: &str, message: &str) -> Response {
    Response::new(
        status,
        "application/json",
        format!(r#"{{"code":"{code}","message":"{message}"}}"#).into_bytes(),
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
    use std::process::Command;

    #[test]
    fn websocket_failures_close_with_size_or_protocol_code() {
        assert_eq!(
            close_code_for_failure(orna_live_v1::Error::Limit),
            1009,
            "an oversized message closes with 1009"
        );
        assert_eq!(
            close_code_for_failure(orna_live_v1::Error::InvalidFrame),
            1002,
            "a malformed frame closes with 1002"
        );
        assert_eq!(
            close_code_for_failure(orna_live_v1::Error::InvalidMessage),
            1002,
            "a malformed message closes with 1002"
        );
    }

    #[test]
    fn accept_retries_transient_failures_and_stops_on_fatal_ones() {
        assert!(is_transient_accept_error(&io::Error::from(
            io::ErrorKind::ConnectionAborted
        )));
        assert!(is_transient_accept_error(&io::Error::from(
            io::ErrorKind::Interrupted
        )));
        assert!(
            is_transient_accept_error(&io::Error::from_raw_os_error(24)),
            "EMFILE retries after descriptors are released"
        );
        assert!(is_transient_accept_error(&io::Error::from_raw_os_error(23)));
        assert!(!is_transient_accept_error(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
        assert!(!is_transient_accept_error(&io::Error::from_raw_os_error(
            22
        )));
    }

    /// Writes `request` from a client socket and returns the server side of
    /// the connection, buffered the way `serve_connection` reads it.
    fn buffered_request(request: &[u8]) -> BufReader<TcpStream> {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        client.write_all(request).unwrap();
        let (server, _) = listener.accept().unwrap();
        BufReader::new(server)
    }

    #[test]
    fn malformed_orna_head_is_answered_in_the_live_error_shape() {
        let mut reader = buffered_request(b"GET /orna/session HTTP/1.1\r\nno-colon\r\n\r\n");
        let mut orna_route = false;
        let deadline = Instant::now() + StdDuration::from_secs(5);
        assert!(read_request_head(&mut reader, deadline, &mut orna_route).is_err());
        assert!(orna_route, "the request line already chose the live route");
        assert_eq!(
            live_error_response(400, "wire.invalid_message", "malformed request").body,
            br#"{"code":"wire.invalid_message","message":"malformed request"}"#.to_vec()
        );
    }

    #[test]
    fn html_pages_carry_csp_nosniff_framing_and_referrer_headers() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=pages"],
        );
        std::fs::write(directory.path().join("main.orna"), SERVE_FIXTURE)
            .expect("vendored project source");
        git_succeeds(directory.path(), &["add", "main.orna"]);
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
                "pages",
            ],
        );
        let repository = Repository::discover(directory.path()).expect("database repository");
        let commit = repository.head().expect("read HEAD").expect("commit");
        let commit_id = commit.as_str();
        let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
        let host = address.to_string();

        for path in [
            format!("/tree/{commit_id}/"),
            format!("/blob/{commit_id}/main.orna"),
        ] {
            let page = exchange(
                address,
                &format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n"),
            );
            assert_eq!(page.status, 200, "{path}");
            assert!(page.head.contains("Content-Type: text/html"), "{path}");
            for header in [
                "Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
                "X-Content-Type-Options: nosniff",
                "X-Frame-Options: DENY",
                "Referrer-Policy: no-referrer",
            ] {
                assert!(page.head.contains(header), "{path} lacks {header}: {}", page.head);
            }
        }
    }

    #[test]
    fn oversized_non_orna_body_is_413_and_foreign_origin_is_refused_before_sizing() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=sizes"],
        );
        let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
        let host = address.to_string();
        let declared = MAX_REQUEST_BODY_BYTES + 1;

        let own = exchange(
            address,
            &format!(
                "POST /tree/x/ HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\nContent-Length: {declared}\r\n\r\n"
            ),
        );
        assert_eq!(own.status, 413, "{}", own.head);
        assert_eq!(own.body, r#"{"error":"payload_too_large"}"#);

        let foreign = exchange(
            address,
            &format!(
                "POST /tree/x/ HTTP/1.1\r\nHost: {host}\r\nOrigin: http://evil.example\r\nContent-Length: {declared}\r\n\r\n"
            ),
        );
        assert_eq!(
            foreign.status, 403,
            "a foreign origin is refused before its body size is judged: {}",
            foreign.head
        );
    }

    #[test]
    fn oversized_orna_body_is_refused_by_its_declared_length() {
        let declared = MAX_REQUEST_BODY_BYTES + 1;
        let head =
            format!("POST /orna/session HTTP/1.1\r\nHost: x\r\nContent-Length: {declared}\r\n\r\n");
        let mut reader = buffered_request(head.as_bytes());
        let mut orna_route = false;
        let deadline = Instant::now() + StdDuration::from_secs(5);
        let (request, body_length) = read_request_head(&mut reader, deadline, &mut orna_route)
            .expect("a well-formed head parses");
        assert!(orna_route);
        assert_eq!(body_length, declared);
        assert!(body_length > request_body_limit(&request.path));
        assert_eq!(
            live_error_response(413, "wire.limit", "request body exceeds its limit").status,
            413
        );
    }

    const SERVE_FIXTURE: &str = include_str!("../tests/fixtures/serve-no-autoload.orna");

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

    fn listing_page(root: &Path, path: &str) -> Response {
        host_route(
            root,
            RuntimeIdentity {
                database_id: [1; 16],
                repository_id: [2; 16],
            },
            false,
            &Request {
                method: "GET".into(),
                path: path.into(),
                query: String::new(),
                headers: Vec::new(),
                body: Vec::new(),
            },
        )
    }

    /// ORNA-SERVE-002: `orna serve` MUST NOT automatically invoke root `main()` or unrelated stream programs.
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
            false,
            &Request {
                method: "GET".into(),
                path: "/".into(),
                query: String::new(),
                headers: Vec::new(),
                body: Vec::new(),
            },
        );
        assert_eq!(home.status, 404, "no native root page without serve");
        assert!(!String::from_utf8_lossy(&home.body).contains("No commits in this repository."));
        let query = host_route(
            directory.path(),
            identity,
            false,
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
    fn listing_routes_serve_a_published_capture_artifact() {
        use orna_repository_v1::{
            ManagedFileChange, ManagedPath, PublicationJournal, PublicationJournalEntry,
        };

        let directory = tempfile::tempdir().expect("temporary database");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=publish"],
        );
        // Commits made by the publication path need an identity that does not
        // depend on the host's global Git configuration.
        git_succeeds(directory.path(), &["config", "user.name", "kierandrewett"]);
        git_succeeds(
            directory.path(),
            &["config", "user.email", "kieran@drewett.dev"],
        );
        std::fs::write(directory.path().join("main.orna"), SERVE_FIXTURE)
            .expect("crate-local Orna source fixture");
        git_succeeds(directory.path(), &["add", "--", "main.orna"]);
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
                "base",
            ],
        );

        let repository = Repository::discover(directory.path()).expect("database repository");
        let head = repository.head().expect("read HEAD").expect("base commit");
        let index_before = repository.index_generation().expect("index generation");
        let artifact = ManagedPath::new("published/capture.orna").unwrap();
        let candidate = repository
            .build_private_commit(
                &head,
                &[ManagedFileChange::new(
                    artifact.clone(),
                    Some(SERVE_FIXTURE.as_bytes().to_vec()),
                )],
                "orna: publish capture artifact",
            )
            .expect("private candidate commit");
        let mut journal = PublicationJournal::new_with_runtime_intent(
            head.clone(),
            candidate.commit().clone(),
            index_before.tree().expect("index tree").clone(),
            [3; 16],
            vec![PublicationJournalEntry::new(
                artifact.clone(),
                None,
                Some(SERVE_FIXTURE.as_bytes().to_vec()),
            )],
        )
        .expect("publication journal");
        repository
            .publish_candidate(&index_before, &candidate, &mut journal)
            .expect("publish capture artifact");
        let published = repository
            .head()
            .expect("read published HEAD")
            .expect("published commit");
        let published_id = published.as_str();

        let tree = listing_page(
            directory.path(),
            &format!("/tree/{published_id}/published/"),
        );
        assert_eq!(tree.status, 200);
        let tree = String::from_utf8(tree.body).expect("published tree HTML");
        assert!(tree.contains(&format!("/blob/{published_id}/published/capture.orna")));

        let blob = listing_page(
            directory.path(),
            &format!("/blob/{published_id}/published/capture.orna"),
        );
        assert_eq!(blob.status, 200);
        let blob = String::from_utf8(blob.body).expect("published blob HTML");
        assert!(blob.contains("orna"));
        assert!(!blob.contains("uncommitted"));
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
        std::fs::write(directory.path().join("<b>&'\"x.txt"), "hostile name")
            .expect("hostile file name");
        std::fs::write(directory.path().join("src/data.bin"), [0, 255])
            .expect("generic binary file");
        git_succeeds(
            directory.path(),
            &[
                "add",
                "--",
                "main.orna",
                "src",
                "README.txt",
                "<b>&'\"x.txt",
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

        let root = listing_page(directory.path(), "/");
        assert_eq!(root.status, 404, "no native root page without serve");

        let tree = listing_page(directory.path(), &format!("/tree/{commit_id}/"));
        assert_eq!(tree.status, 200);
        let tree = String::from_utf8(tree.body).expect("tree HTML");
        assert!(tree.contains(&format!("/tree/{commit_id}/src/")));
        assert!(tree.contains(&format!("/blob/{commit_id}/main.orna")));
        assert!(tree.contains(&format!("/blob/{commit_id}/README.txt")));
        assert!(!tree.contains("uncommitted.txt"));
        assert!(
            tree.contains("&lt;b&gt;&amp;&#39;&quot;x.txt"),
            "tree names are escaped: {tree}"
        );
        assert!(
            !tree.contains("<b>&'\"x.txt"),
            "raw hostile name must not render: {tree}"
        );

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

    /// ORNA-SERVE-003: A different clone running `orna serve` serves that clone's HEAD and CWD; CWD MUST remain local to that clone.
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

    /// A13-live-serving-24: the request head is parsed once, so a request
    /// line split across TCP segments still reaches the WebSocket route.
    #[test]
    fn request_head_reader_accepts_a_fragmented_request_line() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accepted socket");
            read_request_head(
                &mut BufReader::new(stream),
                Instant::now() + StdDuration::from_secs(5),
                &mut false,
            )
        });
        let mut client = TcpStream::connect(address).expect("loopback connection");
        client
            .write_all(b"GET /orna/live/")
            .expect("first fragment");
        std::thread::sleep(StdDuration::from_millis(10));
        client
            .write_all(b"session HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .expect("request line remainder");
        let (request, body_length) = server
            .join()
            .expect("request head thread")
            .expect("complete request head");
        assert_eq!(request.path, "/orna/live/session");
        assert_eq!(body_length, 0);
        assert!(is_websocket_route(&request));
    }

    /// ORNA-WIRE-005, A13-live-serving-24: a peer that never ends its request
    /// line is refused at the header byte bound instead of being buffered or
    /// polled until it stops.
    #[test]
    fn request_head_reader_refuses_an_unterminated_line_at_the_byte_bound() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accepted socket");
            let started = Instant::now();
            let result = read_request_head(
                &mut BufReader::new(stream),
                Instant::now() + StdDuration::from_secs(30),
                &mut false,
            );
            (result.err(), started.elapsed())
        });
        let mut client = TcpStream::connect(address).expect("loopback connection");
        client
            .write_all(&vec![b'a'; MAX_HEADER_BYTES + 1])
            .expect("unterminated request line");
        let (error, elapsed) = server.join().expect("request head thread");
        let error = error.expect("an unterminated line is refused");
        assert_ne!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            elapsed < StdDuration::from_secs(10),
            "the byte bound applies before the deadline: {elapsed:?}"
        );
    }

    /// ORNA-WIRE-005, A13-live-serving-24: a peer that trickles bytes without
    /// ending its request line is cut off at the head deadline.
    #[test]
    fn request_head_reader_stops_a_trickling_peer_at_its_deadline() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("listener");
        let address = listener.local_addr().expect("listener address");
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accepted socket");
            let started = Instant::now();
            let result = read_request_head(
                &mut BufReader::new(stream),
                Instant::now() + StdDuration::from_millis(300),
                &mut false,
            );
            (result.err(), started.elapsed())
        });
        let mut client = TcpStream::connect(address).expect("loopback connection");
        let trickle = std::thread::spawn(move || {
            for _ in 0..100 {
                if client.write_all(b"G").is_err() {
                    break;
                }
                std::thread::sleep(StdDuration::from_millis(20));
            }
        });
        let (error, elapsed) = server.join().expect("request head thread");
        let error = error.expect("a trickling peer is refused");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            elapsed >= StdDuration::from_millis(300) && elapsed < StdDuration::from_secs(2),
            "the head deadline bounds the whole request line: {elapsed:?}"
        );
        trickle.join().expect("trickling client");
    }

    /// ORNA-TRUST-002: `orna serve` binds to loopback by default. Remote deployment SHOULD use normal SSH, Tailscale, or a trusted authenticated reverse proxy/TLS boundary.
    /// ORNA-SERVE-004: The default listener MUST bind only to loopback unless the operator explicitly selects another address.
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
            let state = new_serve_state(
                test_clone(&root, identity),
                status.address,
                ServeOptions::default(),
            )
            .expect("serve host");
            serve_connection(stream, &state).expect("served clone route");
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
            let address = start_live_server(
                source.path(),
                identity,
                ServeOptions {
                    allow_push: true,
                    ..ServeOptions::default()
                },
            );

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

            let report = exchange(
                address,
                &format!("GET /api/clone HTTP/1.1\r\nHost: {address}\r\n\r\n"),
            );
            assert_eq!(report.status, 200);
            assert!(
                report
                    .body
                    .contains(&format!("\"HEAD\":\"{expected_head}\""))
            );
            assert!(report.body.contains(&format!("\"branch\":\"{branch}\"")));
            assert!(
                report
                    .body
                    .contains(&format!("\"worktree_clean\":{}", !dirty))
            );
        }
    }

    /// ORNA-TRUST-003, A13-live-serving-09: smart-HTTP push changes refs in
    /// the served clone, so it is refused unless the operator enables it.
    /// Fetch stays available.
    #[test]
    fn smart_http_push_is_refused_unless_enabled() {
        let source = tempfile::tempdir().expect("source clone");
        std::fs::write(source.path().join("main.orna"), SERVE_FIXTURE)
            .expect("vendored project source");
        git_succeeds(
            source.path(),
            &["init", "--quiet", "--initial-branch=push-off"],
        );
        git_succeeds(source.path(), &["add", "main.orna"]);
        git_succeeds(
            source.path(),
            &[
                "-c",
                "user.name=Orna test",
                "-c",
                "user.email=orna-test@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "base",
            ],
        );
        let address = start_live_server(source.path(), TEST_IDENTITY, ServeOptions::default());
        let remote = format!("http://{address}/git");
        let destination = tempfile::tempdir().expect("client clone parent");
        let clone_path = destination.path().join("checkout");
        let cloned = Command::new("git")
            .args(["-c", "http.proxy=", "clone", "--quiet", &remote])
            .arg(&clone_path)
            .output()
            .expect("git clone");
        assert!(
            cloned.status.success(),
            "fetch stays available: {}",
            String::from_utf8_lossy(&cloned.stderr)
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
                "HEAD:refs/heads/refused",
            ])
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git push");
        assert!(
            !pushed.status.success(),
            "push succeeded without `serve --allow-push`"
        );
        let created = Command::new("git")
            .args(["rev-parse", "--verify", "--quiet", "refs/heads/refused"])
            .current_dir(source.path())
            .output()
            .expect("refused ref lookup");
        assert!(!created.status.success(), "a refused push created a ref");
        let advertisement = exchange(
            address,
            &format!(
                "GET /git/info/refs?service=git-receive-pack HTTP/1.1\r\nHost: {address}\r\n\r\n"
            ),
        );
        assert_eq!(advertisement.status, 403);
        assert_eq!(advertisement.body, r#"{"error":"push_disabled"}"#);
    }

    /// ORNA-WIRE-011, ORNA-TRUST-003, A13-live-serving-09: normal same-origin
    /// checks. Only this listener's loopback names are served, so a rebound
    /// DNS name cannot reach it as a same-origin peer, and writes must come
    /// from the request's own origin.
    #[test]
    fn requests_must_name_this_listener_and_writes_must_be_same_origin() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=hosts"],
        );
        let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
        let port = address.port();
        let own = address.to_string();
        let rebound = format!("rebound.example:{port}");
        let home =
            |host: &str| exchange(address, &format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n"));
        for host in [
            own.clone(),
            format!("localhost:{port}"),
            format!("LOCALHOST:{port}"),
            format!("[::1]:{port}"),
        ] {
            let accepted = home(&host);
            assert_eq!(accepted.status, 404, "{host} names this listener");
            assert_eq!(accepted.body, r#"{"error":"not_found"}"#);
        }
        for host in [
            rebound.clone(),
            format!("127.0.0.1:{}", port.wrapping_add(1)),
            "127.0.0.1".to_owned(),
            "localhost".to_owned(),
        ] {
            let refused = home(&host);
            assert_eq!(refused.status, 403, "{host} does not name this listener");
            assert_eq!(refused.body, r#"{"error":"host_denied"}"#);
        }

        let refused = create_session_as(address, &rebound, Some(&format!("http://{rebound}")));
        assert_eq!(refused.status, 403);
        assert!(
            refused.body.contains(r#""code":"live.host_denied""#),
            "{}",
            refused.body
        );
        for origin in [
            None,
            Some("null".to_owned()),
            Some("http://rebound.example".to_owned()),
            Some(format!("http://localhost:{port}")),
        ] {
            let refused = create_session_as(address, &own, origin.as_deref());
            assert_eq!(
                refused.status, 403,
                "{origin:?} is not this request's origin"
            );
            assert!(
                refused.body.contains(r#""code":"live.origin_denied""#),
                "{}",
                refused.body
            );
        }
        // Each loopback name of this listener is its own origin.
        for host in [own.clone(), format!("localhost:{port}")] {
            let created = create_session_as(address, &host, Some(&format!("http://{host}")));
            assert_eq!(created.status, 201, "{host}: {}", created.body);
        }

        // Git's own client sends no Origin; a page from another origin cannot
        // drive Git's POST endpoints.
        let upload = exchange(
            address,
            &format!(
                "POST /git/git-upload-pack HTTP/1.1\r\nHost: {own}\r\nOrigin: http://rebound.example\r\nContent-Type: application/x-git-upload-pack-request\r\nContent-Length: 0\r\n\r\n"
            ),
        );
        assert_eq!(upload.status, 403);
        assert_eq!(upload.body, r#"{"error":"origin_denied"}"#);

        // The refusal is decided from the request head alone: a cross-origin
        // request that declares a large body and never sends it is answered at
        // once, without the host waiting for or buffering that body.
        let started = Instant::now();
        let declared = exchange(
            address,
            &format!(
                "POST /git/git-upload-pack HTTP/1.1\r\nHost: {own}\r\nOrigin: http://rebound.example\r\nContent-Type: application/x-git-upload-pack-request\r\nContent-Length: 1000000\r\n\r\n"
            ),
        );
        assert_eq!(declared.status, 403);
        assert_eq!(declared.body, r#"{"error":"origin_denied"}"#);
        assert!(
            started.elapsed() < StdDuration::from_secs(5),
            "the refusal does not wait for the declared body: {:?}",
            started.elapsed()
        );

        // A WebSocket handshake through a rebound name never reaches the
        // session layer.
        let session = create_session(address);
        let upgrade = exchange(
            address,
            &format!(
                "GET /orna/live/{} HTTP/1.1\r\nHost: {rebound}\r\nOrigin: http://{rebound}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: orna.present.v1\r\nCookie: orna_session={}\r\n\r\n",
                session.id, session.token
            ),
        );
        assert_eq!(upgrade.status, 403);
        assert!(upgrade.body.contains(r#""code":"live.host_denied""#));
    }

    /// ORNA-WIRE-005, A13-live-serving-01: an attached WebSocket waiting on
    /// its peer must not hold the shared session state. Before per-step
    /// locking, the first socket's thread held it until that socket closed,
    /// so the session request and the second socket below never completed.
    #[test]
    fn an_open_websocket_leaves_session_requests_and_other_sockets_available() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=live"],
        );
        let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
        let first = create_session(address);
        let mut first_socket = open_websocket(address, &first);

        let second = create_session(address);
        let mut second_socket = open_websocket(address, &second);
        assert_expression_result(&eval_over_socket(
            &mut second_socket,
            &second,
            [41; 16],
            EXPRESSION,
        ));
        assert_expression_result(&eval_over_socket(
            &mut first_socket,
            &first,
            [42; 16],
            EXPRESSION,
        ));
    }

    /// 30-protocol §Session creation and ownership, A13-live-serving-01: a
    /// successful resume replaces the old attachment. The replaced socket's
    /// worker is cancelled and acknowledges its retirement when it exits, so
    /// the session is not left fenced against deletion.
    #[test]
    fn resume_replaces_the_attached_socket_and_releases_its_worker() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=resume"],
        );
        let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
        let session = create_session(address);
        let mut replaced = open_websocket(address, &session);

        let resumed = resume_session(address, &session);
        assert_eq!(resumed.id, session.id);
        assert_ne!(resumed.token, session.token);
        // The cancelled worker sends its closing handshake, then disconnects.
        expect_server_close(&mut replaced, CLOSE_RETIRED);

        let mut current = open_websocket(address, &resumed);
        send_frame(&mut current, 8, &1000_u16.to_be_bytes());
        let (opcode, payload) = read_frame(&mut current).expect("close acknowledgement");
        assert_eq!(opcode, 8);
        assert_eq!(payload, 1000_u16.to_be_bytes());
        assert_eq!(delete_session_once_released(address, &resumed), 204);
    }

    /// ORNA-WIRE-005, A13-live-serving-01: a half-open peer cannot hold its
    /// attachment forever. An idle socket is pinged; a peer that answers
    /// stays attached past the idle bound, and one that falls silent is
    /// disconnected and its attachment released.
    #[test]
    fn idle_websockets_are_pinged_and_silent_peers_are_released() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=keepalive"],
        );
        // The idle bound leaves each ping more than two seconds for its
        // answer, so a test thread descheduled for a second on a loaded CI
        // runner is not mistaken for a silent peer.
        let keepalive = WebSocketKeepalive {
            ping_after: StdDuration::from_millis(100),
            idle_timeout: StdDuration::from_millis(2_500),
            write_timeout: StdDuration::from_secs(5),
        };
        let address = start_live_server(
            directory.path(),
            TEST_IDENTITY,
            ServeOptions {
                keepalive,
                ..ServeOptions::default()
            },
        );
        let session = create_session(address);
        let mut socket = open_websocket(address, &session);

        let answering = Instant::now();
        let mut pings = 0;
        while answering.elapsed() < keepalive.idle_timeout + keepalive.idle_timeout / 2 {
            let (opcode, payload) = read_frame(&mut socket).expect("keepalive ping");
            assert_eq!(opcode, 9, "an idle socket is pinged");
            send_frame(&mut socket, 10, &payload);
            pings += 1;
        }
        assert!(pings > 1, "a peer that answers pings stays attached");

        let (opcode, _) = read_frame(&mut socket).expect("final keepalive ping");
        assert_eq!(opcode, 9);
        // A silent peer is told the host is going away, then disconnected.
        expect_server_close(&mut socket, CLOSE_IDLE);
        assert_eq!(delete_session_once_released(address, &session), 204);
    }

    /// ORNA-WIRE-005, A13-live-serving-01: the keepalive judges the peer, not
    /// the host. An evaluation that outlasts the idle bound (here, waiting on
    /// the application lock) delivers its result and leaves the socket
    /// attached for the next request.
    #[test]
    fn evaluation_time_is_not_counted_as_peer_silence() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=busy"],
        );
        // The idle bound leaves the client two seconds to send its next
        // request, so a test thread descheduled for a second is not mistaken
        // for a silent peer. The evaluation below is held for longer than it.
        let keepalive = WebSocketKeepalive {
            ping_after: StdDuration::from_millis(100),
            idle_timeout: StdDuration::from_secs(2),
            write_timeout: StdDuration::from_secs(5),
        };
        let (address, state) = start_shared_live_server(
            directory.path(),
            TEST_IDENTITY,
            ServeOptions {
                keepalive,
                ..ServeOptions::default()
            },
        );
        let session = create_session(address);
        let mut socket = open_websocket(address, &session);

        let busy = state.application.lock().expect("live application");
        let waiting = std::thread::spawn(move || {
            let result = eval_over_socket(&mut socket, &session, [51; 16], EXPRESSION);
            (socket, session, result)
        });
        std::thread::sleep(keepalive.idle_timeout + StdDuration::from_secs(1));
        drop(busy);
        let (mut socket, session, result) = waiting.join().expect("Eval client");
        assert_expression_result(&result);
        assert_expression_result(&eval_over_socket(
            &mut socket,
            &session,
            [52; 16],
            EXPRESSION,
        ));
    }

    /// 30-protocol §Session creation and ownership, A13-live-serving-15:
    /// DELETE ends a session whose socket is still attached and which owns
    /// application work. It cancels the socket's worker, which sends its
    /// closing handshake and disconnects, and answers 204 only after that
    /// worker has exited. Before, DELETE answered 503 while a socket was
    /// attached and 400 once the session had run an evaluation.
    #[test]
    fn delete_ends_an_attached_session_after_its_socket_worker_exits() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=delete"],
        );
        let (address, state) =
            start_shared_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
        let session = create_session(address);
        let mut socket = open_websocket(address, &session);
        // The result also shows that the attachment has been committed.
        assert_expression_result(&eval_over_socket(
            &mut socket,
            &session,
            [61; 16],
            EXPRESSION,
        ));

        let deleted = delete_session(address, &session);
        assert_eq!(deleted.status, 204, "{}", deleted.body);
        assert!(
            state.workers.lock().is_empty(),
            "the socket's worker exits before the 204 is written"
        );
        expect_server_close(&mut socket, CLOSE_RETIRED);
        assert_eq!(resume_reply(address, &session).status, 410);
    }

    /// 30-protocol §Session creation and ownership, W20-04: a session that
    /// has run an evaluation can be deleted once its socket has gone. DELETE
    /// joins its finished application work and answers 204; before, it
    /// answered 400 `live.malformed_request`.
    #[test]
    fn delete_ends_a_detached_session_that_has_evaluated() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=detached"],
        );
        let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
        let session = create_session(address);
        let mut socket = open_websocket(address, &session);
        assert_expression_result(&eval_over_socket(
            &mut socket,
            &session,
            [81; 16],
            EXPRESSION,
        ));
        send_frame(&mut socket, 8, &1000_u16.to_be_bytes());
        let (opcode, _) = read_frame(&mut socket).expect("close acknowledgement");
        assert_eq!(opcode, 8);
        assert!(closed_by_server(&mut socket));

        assert_eq!(delete_session_once_released(address, &session), 204);
        assert_eq!(resume_reply(address, &session).status, 410);
    }

    /// 30-protocol §Session creation and ownership, A13-live-serving-15:
    /// expiry terminates the owner and cancels its children. Once a session's
    /// lease elapses, the owner sweep ends it without any client request, so
    /// its attached socket is closed and its token can no longer resume it.
    #[test]
    fn expired_sessions_are_ended_by_the_owner_sweep() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=expiry"],
        );
        // Keepalive pings start only after 15 s and the client waits at most
        // LIVE_TEST_TIMEOUT for the Close frame. The reconnection lease starts
        // when the socket detaches (30-protocol), so the session is ended only
        // after the client closes; the lease leaves the evaluation four seconds.
        let address = start_live_server(
            directory.path(),
            TEST_IDENTITY,
            ServeOptions {
                session_lease_ms: 4_000,
                ..ServeOptions::default()
            },
        );
        let session = create_session(address);
        let mut socket = open_websocket(address, &session);
        assert_expression_result(&eval_over_socket(
            &mut socket,
            &session,
            [71; 16],
            EXPRESSION,
        ));
        // Closing the socket detaches it, starting the reconnection lease.
        send_frame(&mut socket, 8, &1000_u16.to_be_bytes());
        let (opcode, _) = read_frame(&mut socket).expect("close acknowledgement");
        assert_eq!(opcode, 8);
        let deadline = Instant::now() + LIVE_TEST_TIMEOUT;
        loop {
            let reply = resume_reply(address, &session);
            if reply.status == 410 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the owner sweep ends the detached session"
            );
            std::thread::sleep(StdDuration::from_millis(50));
        }
        assert_eq!(resume_reply(address, &session).status, 410);
    }

    /// ORNA-WIRE-005, A13-live-serving-13: the connections being served,
    /// each on its own thread, are bounded. Beyond the cap the accept loop
    /// answers 503 at once without reading the request, and a connection's
    /// slot is free again once it has ended.
    #[test]
    fn connections_beyond_the_cap_are_refused_until_one_ends() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=cap"],
        );
        let address = start_live_server(
            directory.path(),
            TEST_IDENTITY,
            ServeOptions {
                max_connections: 1,
                ..ServeOptions::default()
            },
        );
        let home = || {
            exchange(
                address,
                &format!("GET / HTTP/1.1\r\nHost: {address}\r\n\r\n"),
            )
        };
        // A request's slot is released before its peer sees the response end,
        // so the handshake that follows on a new connection is admitted.
        let session = create_session(address);
        let mut socket = open_websocket(address, &session);

        let refused = home();
        assert_eq!(refused.status, 503);
        assert_eq!(refused.body, r#"{"error":"temporarily_unavailable"}"#);

        send_frame(&mut socket, 8, &1000_u16.to_be_bytes());
        let (opcode, _) = read_frame(&mut socket).expect("close acknowledgement");
        assert_eq!(opcode, 8);
        assert!(closed_by_server(&mut socket));
        // The socket's worker ends the connection just before its thread
        // releases the slot.
        let deadline = Instant::now() + LIVE_TEST_TIMEOUT;
        loop {
            let reply = home();
            if reply.status != 503 {
                assert_eq!(reply.status, 404, "{}", reply.body);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the ended connection's slot is released"
            );
            std::thread::sleep(StdDuration::from_millis(20));
        }
    }

    /// 30-protocol:15: the listener is plaintext HTTP, so the session cookie
    /// must not carry `Secure`, which a browser would not return over http.
    #[test]
    fn session_cookie_is_not_secure_over_plain_http() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=cookie"],
        );
        let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
        let reply = create_session_as(
            address,
            &address.to_string(),
            Some(&format!("http://{address}")),
        );
        assert_eq!(reply.status, 201, "{}", reply.body);
        let cookie = reply
            .head
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("set-cookie:"))
            .expect("session cookie");
        assert!(cookie.contains("HttpOnly"), "{cookie}");
        assert!(!cookie.contains("Secure"), "{cookie}");
    }

    /// 30-protocol:21: a connection refused by the cap on a `/orna/` route is
    /// answered in the protocol's `{ "code", "message" }` shape.
    #[test]
    fn over_cap_refusal_on_orna_routes_uses_the_protocol_error_shape() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=refusal"],
        );
        let address = start_live_server(
            directory.path(),
            TEST_IDENTITY,
            ServeOptions {
                max_connections: 1,
                ..ServeOptions::default()
            },
        );
        // The open socket holds the only slot for as long as it stays attached.
        let session = create_session(address);
        let _socket = open_websocket(address, &session);

        let refused = exchange(
            address,
            &format!(
                "GET /orna/live/{} HTTP/1.1\r\nHost: {address}\r\n\r\n",
                session.id
            ),
        );
        assert_eq!(refused.status, 503);
        assert_eq!(
            refused.body,
            r#"{"code":"live.unavailable","message":"request rejected"}"#
        );
    }

    /// A silent peer refused over the cap must not delay the next refusal:
    /// the accept loop answers the second peer without waiting out the first
    /// peer's head timeout.
    #[test]
    fn silent_refused_peer_does_not_delay_the_next_refusal() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=silent"],
        );
        let address = start_live_server(
            directory.path(),
            TEST_IDENTITY,
            ServeOptions {
                max_connections: 1,
                ..ServeOptions::default()
            },
        );
        let session = create_session(address);
        let _socket = open_websocket(address, &session);
        // Sends nothing, so its refusal runs until REFUSAL_HEAD_TIMEOUT.
        let _silent = std::net::TcpStream::connect(address).expect("silent peer");

        let started = Instant::now();
        let refused = exchange(
            address,
            &format!(
                "GET /orna/live/{} HTTP/1.1\r\nHost: {address}\r\n\r\n",
                session.id
            ),
        );
        assert_eq!(refused.status, 503);
        assert!(
            started.elapsed() < REFUSAL_HEAD_TIMEOUT,
            "a silent peer delayed the next refusal"
        );
    }

    /// 30-protocol:21: once every refusal thread is busy, a connection over
    /// the cap is still answered from the accept loop. A `/orna/` route gets
    /// the protocol error shape, and a generic route gets the generic 503,
    /// rather than a silent close.
    #[test]
    fn saturated_refusal_still_answers_with_the_route_shape() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=saturated"],
        );
        let address = start_live_server(
            directory.path(),
            TEST_IDENTITY,
            ServeOptions {
                max_connections: 1,
                ..ServeOptions::default()
            },
        );
        let session = create_session(address);
        let _socket = open_websocket(address, &session);
        // Silent peers fill every refusal thread; each holds its thread until
        // REFUSAL_HEAD_TIMEOUT. They are accepted in connect order, so the
        // probes below arrive after the pool is full.
        let _silent: Vec<std::net::TcpStream> = (0..MAX_REFUSALS_IN_FLIGHT)
            .map(|_| std::net::TcpStream::connect(address).expect("silent peer"))
            .collect();

        let refused = exchange(
            address,
            &format!(
                "GET /orna/live/{} HTTP/1.1\r\nHost: {address}\r\n\r\n",
                session.id
            ),
        );
        assert_eq!(refused.status, 503);
        assert_eq!(
            refused.body,
            r#"{"code":"live.unavailable","message":"request rejected"}"#
        );

        let generic = exchange(
            address,
            &format!("GET / HTTP/1.1\r\nHost: {address}\r\n\r\n"),
        );
        assert_eq!(generic.status, 503);
        assert_eq!(generic.body, r#"{"error":"temporarily_unavailable"}"#);
    }

    /// ORNA-WIRE-005: a request body must arrive within one transfer budget
    /// measured over the whole body. A peer that trickles a declared body a
    /// byte at a time is answered 400 at the deadline, instead of keeping its
    /// connection thread for as long as it keeps sending.
    #[test]
    fn a_trickled_request_body_is_cut_off_at_its_deadline() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=trickle"],
        );
        let transfer = TransferBudget {
            base: StdDuration::from_millis(500),
            min_bytes_per_second: u64::MAX,
        };
        let address = start_live_server(
            directory.path(),
            TEST_IDENTITY,
            ServeOptions {
                transfer,
                ..ServeOptions::default()
            },
        );
        let started = Instant::now();
        let mut client = TcpStream::connect(address).expect("loopback connection");
        client
            .set_read_timeout(Some(LIVE_TEST_TIMEOUT))
            .expect("client read timeout");
        write!(
            client,
            "POST /orna/session HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n"
        )
        .expect("request head");
        // At one byte every 100 ms the body would take ten seconds.
        let mut trickle = client.try_clone().expect("second client handle");
        let trickler = std::thread::spawn(move || {
            for _ in 0..100 {
                if trickle.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(StdDuration::from_millis(100));
            }
        });
        let response = read_until_closed(&mut client);
        let elapsed = started.elapsed();
        assert!(
            response.starts_with(b"HTTP/1.1 400 "),
            "{}",
            String::from_utf8_lossy(&response)
        );
        assert!(
            elapsed >= transfer.base && elapsed < StdDuration::from_secs(5),
            "the deadline bounds the whole body: {elapsed:?}"
        );
        trickler.join().expect("trickling client");
    }

    /// ORNA-WIRE-005: a response must be taken within one transfer budget
    /// measured over the whole response. A client that stops reading a large
    /// page cannot keep its connection thread: the server gives up at the
    /// deadline, and the client later finds the page cut short.
    #[test]
    fn a_response_the_client_stops_reading_is_abandoned_at_its_deadline() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=stalled"],
        );
        // Each quote renders as the six bytes of `&quot;`, so the page is
        // about 12 MB, more than the loopback socket buffers hold.
        std::fs::write(directory.path().join("quotes.txt"), vec![b'"'; 2_000_000])
            .expect("large text file");
        git_succeeds(directory.path(), &["add", "--", "quotes.txt"]);
        git_succeeds(
            directory.path(),
            &[
                "-c",
                "user.name=Orna test",
                "-c",
                "user.email=orna-test@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "large page",
            ],
        );
        let commit = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(directory.path())
            .output()
            .expect("HEAD")
            .stdout;
        let commit = String::from_utf8(commit).expect("HEAD UTF-8");
        let transfer = TransferBudget {
            base: StdDuration::from_millis(500),
            min_bytes_per_second: u64::MAX,
        };
        let address = start_live_server(
            directory.path(),
            TEST_IDENTITY,
            ServeOptions {
                transfer,
                ..ServeOptions::default()
            },
        );
        let mut client = TcpStream::connect(address).expect("loopback connection");
        client
            .set_read_timeout(Some(LIVE_TEST_TIMEOUT))
            .expect("client read timeout");
        write!(
            client,
            "GET /blob/{}/quotes.txt HTTP/1.1\r\nHost: {address}\r\n\r\n",
            commit.trim()
        )
        .expect("request");
        // Read the head only, then stop reading until well past the deadline.
        let mut head = Vec::new();
        let mut byte = [0; 1];
        while !head.ends_with(b"\r\n\r\n") {
            client.read_exact(&mut byte).expect("response head");
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).expect("UTF-8 response head");
        assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
        let declared: usize = head
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .and_then(|length| length.parse().ok())
            .expect("declared body length");
        assert!(
            declared > 10_000_000,
            "the page outgrows the socket buffers"
        );
        std::thread::sleep(transfer.base * 4);
        let body = read_until_closed(&mut client);
        assert!(
            body.len() < declared,
            "the server stops writing at its deadline: {} of {declared} bytes",
            body.len()
        );
    }

    /// ORNA-EVAL-006 and the 30-protocol message registry ("A result for
    /// success carries the complete typed value, including Unit"),
    /// A13-live-serving-03: a REPL declaration over `orna serve` is answered
    /// with typed Unit, and its binding persists for the session. Before, the
    /// declaration's result was a bare null that could not be encoded, so the
    /// client lost its answer while the server kept the binding.
    #[test]
    fn repl_declarations_over_serve_return_unit_and_persist() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=declarations"],
        );
        let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
        let session = create_session(address);
        let mut socket = open_websocket(address, &session);
        assert_success_value(
            &eval_over_socket(&mut socket, &session, [91; 16], "let x = 41;"),
            &CanonicalValue::unit(),
        );
        assert_success_value(
            &eval_over_socket(&mut socket, &session, [92; 16], "x + 1"),
            &int_value(42),
        );
    }

    /// ORNA-EVAL-006, ORNA-APP-010 and 30-protocol ("An eval source is
    /// exactly one REPL input"), A14-04: console input that declares
    /// `fn serve` is an ordinary REPL declaration. Before, `orna serve`
    /// admitted any Eval declaring a function named `serve` as a one-shot
    /// module and ran it, so the declaration answered with the function's
    /// result and was never retained by the session.
    #[test]
    fn console_fn_serve_is_an_ordinary_declaration() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=console-serve"],
        );
        let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
        let session = create_session(address);
        let mut socket = open_websocket(address, &session);
        assert_success_value(
            &eval_over_socket(&mut socket, &session, [93; 16], "fn serve(): Int = 1;"),
            &CanonicalValue::unit(),
        );
        assert_success_value(
            &eval_over_socket(&mut socket, &session, [94; 16], "serve()"),
            &int_value(1),
        );
    }

    /// 30-protocol message registry, type 2 ("Host replies with the latest
    /// full snapshot"; "explicit resync receives a snapshot with its
    /// originating request ID"), ORNA-LIVE-004, A13-live-serving-08: after
    /// the watched value changes, an explicit resync is answered with the
    /// complete current tree at the next revision. Before, it was answered
    /// with a request-less delta against a base the resyncing client had lost.
    #[test]
    fn resync_after_a_value_change_returns_a_snapshot_with_its_request_id() {
        let directory = tempfile::tempdir().expect("temporary clone");
        git_succeeds(
            directory.path(),
            &["init", "--quiet", "--initial-branch=resync"],
        );
        let address = start_live_server(directory.path(), TEST_IDENTITY, ServeOptions::default());
        let session = create_session(address);
        let mut socket = open_websocket(address, &session);
        assert_success_value(
            &eval_over_socket(&mut socket, &session, [95; 16], "let x = 41;"),
            &CanonicalValue::unit(),
        );
        let watched = watch_over_socket(&mut socket, &session, [96; 16], "x + 1");
        assert_eq!(watched.request, Some([96; 16]));
        let watch = watched.watch.expect("a watch snapshot carries its handle");
        let Message::Snapshot {
            revision: 0,
            present,
            ..
        } = watched.message
        else {
            panic!("a new watch starts with a revision-zero snapshot");
        };
        assert_eq!(
            present,
            orna_protocol_v1::PresentNode::from_value(int_value(42)).expect("watched tree")
        );

        assert_success_value(
            &eval_over_socket(&mut socket, &session, [97; 16], "let x = 1;"),
            &CanonicalValue::unit(),
        );
        let refreshed = resync_over_socket(&mut socket, &session, [98; 16], watch);
        assert_eq!(
            refreshed.request,
            Some([98; 16]),
            "the snapshot names its resync"
        );
        assert_eq!(refreshed.watch, Some(watch));
        let Message::Snapshot {
            revision, present, ..
        } = refreshed.message
        else {
            panic!("an explicit resync is answered with a full snapshot");
        };
        assert_eq!(revision, 1, "the changed tree advances the watch revision");
        assert_eq!(
            present,
            orna_protocol_v1::PresentNode::from_value(int_value(2)).expect("refreshed tree")
        );
    }

    /// G-4: the served runtime is opened under this process's owner, the same
    /// owner `orna run`, the REPL and mounts use in this process. It holds the
    /// worktree's runtime owner lock and the writer lease for that owner at
    /// epoch 1. Re-acquisition is idempotent, and a different owner is refused
    /// even through the served runtime that holds the lock.
    #[test]
    fn served_runtime_holds_the_process_owner_lease() {
        let directory = tempfile::tempdir().expect("temporary project");
        orna_repository_v1::initialize_repository(directory.path()).expect("orna init");
        std::fs::write(
            directory.path().join("main.orna"),
            "pub table Note(id: Int) { text: Str, }\n",
        )
        .expect("project source");
        let endpoint = Endpoint::Path(
            directory
                .path()
                .to_str()
                .expect("UTF-8 project path")
                .to_owned(),
        );
        let clone = open_served_clone(&endpoint).expect("`orna serve` resolves the clone");
        let owner = invocation_owner_id();
        let executor = current_thread_executor().expect("test executor");
        let lease = executor
            .block_on(clone.runtime.acquire_lease(owner))
            .expect("the repository owner acquires the served lease");
        assert_eq!(lease.owner_id, owner);
        assert_eq!(lease.epoch, 1);
        assert!(clone.runtime.holds_runtime_owner_lock());
        assert_eq!(
            executor
                .block_on(clone.runtime.acquire_lease(owner))
                .expect("the holder re-acquires its own lease"),
            lease
        );
        assert!(matches!(
            executor.block_on(clone.runtime.acquire_lease([7; 16])),
            Err(orna_runtime_v1::RuntimeError::LeaseHeld)
        ));
        assert_eq!(
            executor
                .block_on(clone.runtime.current_lease())
                .expect("read the served lease"),
            Some(lease)
        );
    }

    /// ORNA-EVAL-003, ORNA-EVAL-008, ORNA-PROTO-004 and 30-protocol REQUEST-1
    /// (A13-live-serving-04): `orna serve` hosts the clone's durable runtime.
    /// An Eval that inserts a row into a project table commits it once,
    /// together with its terminal outcome, and resending the same request ID
    /// returns that recorded outcome without writing again. Before, the live
    /// host had no runtime state and refused every mutating Eval.
    #[test]
    fn served_eval_insert_commits_once_and_replays_its_recorded_outcome() {
        let directory = tempfile::tempdir().expect("temporary project");
        orna_repository_v1::initialize_repository(directory.path()).expect("orna init");
        std::fs::write(
            directory.path().join("main.orna"),
            "pub table Note(id: Int) { text: Str, }\n",
        )
        .expect("project source");
        let endpoint = Endpoint::Path(
            directory
                .path()
                .to_str()
                .expect("UTF-8 project path")
                .to_owned(),
        );
        let clone = open_served_clone(&endpoint).expect("`orna serve` resolves the clone");
        let database = clone.identity.database_id;
        let (address, _state) = serve_clone(clone, ServeOptions::default());
        let session = create_session_in(address, database);
        let mut socket = open_websocket(address, &session);
        let insert = r#"main.Note.insert({ id: 1, text: "served" })"#;
        let row = CanonicalValue::new(OvbRaw::Map(vec![
            (OvbRaw::Text("id".into()), OvbRaw::Int(1.into())),
            (OvbRaw::Text("text".into()), OvbRaw::Text("served".into())),
        ]))
        .expect("canonical row");

        let committed = eval_over_socket(&mut socket, &session, [99; 16], insert);
        assert_success_value(&committed, &row);
        let rows = committed_rows(directory.path(), "Note");
        assert_eq!(rows.len(), 1, "the insert commits exactly one row");
        assert_eq!(
            CanonicalValue::decode(&rows[0].1).expect("committed row value"),
            row
        );

        let replayed = eval_over_socket(&mut socket, &session, [99; 16], insert);
        assert_eq!(
            replayed, committed,
            "the same request ID returns its recorded outcome"
        );
        assert_eq!(
            committed_rows(directory.path(), "Note"),
            rows,
            "the replay does not write again"
        );
    }

    /// The committed rows of `table`, read through a second handle on the
    /// clone's runtime state, as another local command would read them.
    fn committed_rows(root: &Path, table: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
        let repository = Repository::discover(root).expect("served clone");
        let (identity, initial_digest) =
            runtime_identity(&repository).expect("clone runtime identity");
        current_thread_executor()
            .expect("runtime executor")
            .block_on(async {
                RuntimeState::open(&repository, identity, initial_digest)
                    .await
                    .expect("reopen the clone runtime")
                    .committed_table_rows(table)
                    .await
                    .expect("committed rows")
            })
    }

    const LIVE_TEST_TIMEOUT: StdDuration = StdDuration::from_secs(10);
    const TEST_IDENTITY: RuntimeIdentity = RuntimeIdentity {
        database_id: [31; 16],
        repository_id: [32; 16],
    };
    /// Initial runtime digest of a test clone served under a fixed identity.
    const TEST_INITIAL_DIGEST: [u8; 32] = [33; 32];
    /// A pure console expression, answered by [`assert_expression_result`].
    const EXPRESSION: &str = "40 + 2";

    /// The Git worktree at `root` as `orna serve` hosts it, under a fixed
    /// identity, with the core catalogue and no loaded project.
    fn test_clone(root: &Path, identity: RuntimeIdentity) -> ServedClone {
        let repository = Repository::discover(root).expect("served Git worktree");
        let runtime = current_thread_executor()
            .expect("runtime executor")
            .block_on(RuntimeState::open(
                &repository,
                identity,
                TEST_INITIAL_DIGEST,
            ))
            .expect("clone runtime state");
        ServedClone {
            root: root.to_path_buf(),
            identity,
            catalogue: orna_semantic_v1::Catalogue::authoritative_core(),
            project: None,
            app: None,
            runtime,
            table_candidate_validator: None,
        }
    }

    /// Starts the production accept loop on its own thread, exactly as
    /// `orna serve` runs it. The thread lives until the test process exits.
    fn start_live_server(
        root: &Path,
        identity: RuntimeIdentity,
        options: ServeOptions,
    ) -> SocketAddr {
        start_shared_live_server(root, identity, options).0
    }

    /// [`start_live_server`], also returning the state the listener shares
    /// with its connection threads.
    fn start_shared_live_server(
        root: &Path,
        identity: RuntimeIdentity,
        options: ServeOptions,
    ) -> (SocketAddr, Arc<ServeState>) {
        serve_clone(test_clone(root, identity), options)
    }

    /// Serves `clone` from the production accept loop on its own thread and
    /// returns its address and the state the listener shares with its
    /// connection threads.
    fn serve_clone(clone: ServedClone, options: ServeOptions) -> (SocketAddr, Arc<ServeState>) {
        let listener = LiveTransport::bind_default_listener(0).expect("loopback listener");
        let address = listener.status().address;
        let state = Arc::new(new_serve_state(clone, address, options).expect("serve host"));
        let shared = Arc::clone(&state);
        std::thread::spawn(move || serve_listener(listener.listener(), &shared));
        (address, state)
    }

    struct Reply {
        status: u16,
        head: String,
        body: String,
    }

    fn exchange(address: SocketAddr, request: &str) -> Reply {
        let mut stream = TcpStream::connect(address).expect("loopback connection");
        stream
            .set_read_timeout(Some(LIVE_TEST_TIMEOUT))
            .expect("client read timeout");
        stream.write_all(request.as_bytes()).expect("request bytes");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .expect("the server answers before the client timeout");
        let response = String::from_utf8(response).expect("UTF-8 response");
        let (head, body) = response
            .split_once("\r\n\r\n")
            .expect("complete response head");
        let status = head
            .split(' ')
            .nth(1)
            .and_then(|code| code.parse().ok())
            .expect("response status");
        Reply {
            status,
            head: head.to_owned(),
            body: body.to_owned(),
        }
    }

    struct LiveSession {
        id: String,
        token: String,
        database: [u8; 16],
    }

    fn post_json(
        address: SocketAddr,
        host: &str,
        origin: Option<&str>,
        path: &str,
        body: &str,
    ) -> Reply {
        let origin = origin
            .map(|origin| format!("Origin: {origin}\r\n"))
            .unwrap_or_default();
        exchange(
            address,
            &format!(
                "POST {path} HTTP/1.1\r\nHost: {host}\r\n{origin}Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
    }

    fn create_session_as(address: SocketAddr, host: &str, origin: Option<&str>) -> Reply {
        create_database_session_as(address, host, origin, TEST_IDENTITY.database_id)
    }

    fn create_database_session_as(
        address: SocketAddr,
        host: &str,
        origin: Option<&str>,
        database: [u8; 16],
    ) -> Reply {
        post_json(
            address,
            host,
            origin,
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                format_uuid(database)
            ),
        )
    }

    fn live_session(reply: &Reply) -> LiveSession {
        let body: serde_json::Value = serde_json::from_str(&reply.body).expect("session JSON");
        LiveSession {
            id: body["session"].as_str().expect("session id").to_owned(),
            token: body["resume_token"]
                .as_str()
                .expect("resume token")
                .to_owned(),
            database: uuid_bytes(body["database"].as_str().expect("session database")),
        }
    }

    fn create_session(address: SocketAddr) -> LiveSession {
        create_session_in(address, TEST_IDENTITY.database_id)
    }

    fn create_session_in(address: SocketAddr, database: [u8; 16]) -> LiveSession {
        let host = address.to_string();
        let reply =
            create_database_session_as(address, &host, Some(&format!("http://{host}")), database);
        assert_eq!(reply.status, 201, "{}", reply.body);
        live_session(&reply)
    }

    fn resume_reply(address: SocketAddr, session: &LiveSession) -> Reply {
        let host = address.to_string();
        post_json(
            address,
            &host,
            Some(&format!("http://{host}")),
            &format!("/orna/session/{}/resume", session.id),
            &format!(
                r#"{{"resume_token":"{}","protocol":"orna.present.v1"}}"#,
                session.token
            ),
        )
    }

    fn resume_session(address: SocketAddr, session: &LiveSession) -> LiveSession {
        let reply = resume_reply(address, session);
        assert_eq!(reply.status, 200, "{}", reply.body);
        live_session(&reply)
    }

    fn delete_session(address: SocketAddr, session: &LiveSession) -> Reply {
        let host = address.to_string();
        exchange(
            address,
            &format!(
                "DELETE /orna/session/{} HTTP/1.1\r\nHost: {host}\r\nOrigin: http://{host}\r\nAuthorization: Bearer {}\r\n\r\n",
                session.id, session.token
            ),
        )
    }

    /// Deletion answers 503 while a retired socket worker of the session has
    /// not yet acknowledged its retirement, so retry until it settles.
    fn delete_session_once_released(address: SocketAddr, session: &LiveSession) -> u16 {
        let deadline = Instant::now() + LIVE_TEST_TIMEOUT;
        loop {
            let status = delete_session(address, session).status;
            if status != 503 || Instant::now() >= deadline {
                return status;
            }
            std::thread::sleep(StdDuration::from_millis(20));
        }
    }

    fn open_websocket(address: SocketAddr, session: &LiveSession) -> TcpStream {
        let mut stream = TcpStream::connect(address).expect("loopback connection");
        stream
            .set_read_timeout(Some(LIVE_TEST_TIMEOUT))
            .expect("client read timeout");
        write!(
            stream,
            "GET /orna/live/{} HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: orna.present.v1\r\nCookie: orna_session={}\r\n\r\n",
            session.id, session.token
        )
        .expect("upgrade request");
        let mut head = Vec::new();
        let mut byte = [0; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream
                .read_exact(&mut byte)
                .expect("upgrade response before the client timeout");
            head.push(byte[0]);
        }
        let head = String::from_utf8(head).expect("UTF-8 upgrade response");
        assert!(head.starts_with("HTTP/1.1 101 "), "upgrade refused: {head}");
        stream
    }

    fn send_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) {
        let mask = [0x11, 0x22, 0x33, 0x44];
        let mut frame = vec![0x80 | opcode];
        if let Ok(length) = u8::try_from(payload.len())
            && length < 126
        {
            frame.push(0x80 | length);
        } else if let Ok(length) = u16::try_from(payload.len()) {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&length.to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            let length = u64::try_from(payload.len()).expect("frame length");
            frame.extend_from_slice(&length.to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % 4]),
        );
        stream.write_all(&frame).expect("client frame");
    }

    fn read_frame(stream: &mut TcpStream) -> io::Result<(u8, Vec<u8>)> {
        let mut head = [0; 2];
        stream.read_exact(&mut head)?;
        let length = match head[1] & 0x7f {
            126 => {
                let mut length = [0; 2];
                stream.read_exact(&mut length)?;
                usize::from(u16::from_be_bytes(length))
            }
            127 => {
                let mut length = [0; 8];
                stream.read_exact(&mut length)?;
                usize::try_from(u64::from_be_bytes(length)).expect("frame length")
            }
            length => usize::from(length),
        };
        let mut payload = vec![0; length];
        stream.read_exact(&mut payload)?;
        Ok((head[0] & 0x0f, payload))
    }

    /// Whether the server has closed the socket: end of stream or a reset,
    /// but not a client read timeout.
    fn closed_by_server(stream: &mut TcpStream) -> bool {
        let mut byte = [0; 1];
        match stream.read(&mut byte) {
            Ok(read) => read == 0,
            Err(error) => matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
            ),
        }
    }

    /// Reads until the server ends the connection, by end of stream or reset.
    fn read_until_closed(stream: &mut TcpStream) -> Vec<u8> {
        let mut received = Vec::new();
        let mut chunk = [0; 64 * 1024];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => return received,
                Ok(read) => received.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted
                    ) =>
                {
                    return received;
                }
                Err(error) => panic!("the server ends the connection in time: {error}"),
            }
        }
    }

    /// Reads the server's closing handshake: one Close frame carrying `code`,
    /// then the end of the connection.
    fn expect_server_close(stream: &mut TcpStream, code: u16) {
        let (opcode, payload) = read_frame(stream).expect("server Close frame");
        assert_eq!(opcode, 8, "the server sends a Close frame");
        assert_eq!(payload, code.to_be_bytes());
        assert!(
            closed_by_server(stream),
            "the server disconnects after its Close frame"
        );
    }

    fn uuid_bytes(text: &str) -> [u8; 16] {
        let digits = text.replace('-', "");
        let mut bytes = [0; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&digits[index * 2..index * 2 + 2], 16).expect("UUID hex");
        }
        bytes
    }

    /// Sends one client request over `stream` and returns the host's answer.
    /// An Eval carries its request fingerprint, as a conforming client sends
    /// it; other requests need not.
    fn request_over_socket(
        stream: &mut TcpStream,
        session: &LiveSession,
        mut envelope: Envelope,
    ) -> Envelope {
        let limits = ProtocolLimits::default();
        let fingerprint = orna_protocol_v1::canonical_request_fingerprint(
            uuid_bytes(&session.id),
            &envelope,
            limits,
        )
        .expect("request fingerprint");
        if let Message::Eval {
            fingerprint: sent, ..
        } = &mut envelope.message
        {
            *sent = fingerprint;
        }
        send_frame(
            stream,
            2,
            &envelope.encode(limits).expect("canonical request"),
        );
        loop {
            let (opcode, payload) = read_frame(stream).expect("answer before the client timeout");
            // A conforming client answers keepalive pings while it waits.
            if opcode == 9 {
                send_frame(stream, 10, &payload);
                continue;
            }
            assert_eq!(
                opcode, 2,
                "the answer is one binary message, not opcode {opcode} with payload {payload:?}"
            );
            return Envelope::decode(&payload, limits).expect("canonical answer");
        }
    }

    fn session_database(session: &LiveSession) -> orna_protocol_v1::DatabaseContext {
        orna_protocol_v1::DatabaseContext {
            database: session.database,
            snapshot: None,
        }
    }

    fn terminal_presentation() -> PresentationContext {
        PresentationContext {
            locale: "en".into(),
            timezone: None,
            width: None,
            theme: "terminal/default".into(),
            supported_kinds: vec!["text".into(), "group".into()],
        }
    }

    fn eval_over_socket(
        stream: &mut TcpStream,
        session: &LiveSession,
        request: [u8; 16],
        source: &str,
    ) -> Envelope {
        request_over_socket(
            stream,
            session,
            Envelope {
                request: Some(request),
                watch: None,
                message: Message::Eval {
                    source: source.to_owned(),
                    database: session_database(session),
                    presentation: terminal_presentation(),
                    fingerprint: [0; 32],
                },
                extensions: BTreeMap::new(),
            },
        )
    }

    fn watch_over_socket(
        stream: &mut TcpStream,
        session: &LiveSession,
        request: [u8; 16],
        source: &str,
    ) -> Envelope {
        request_over_socket(
            stream,
            session,
            Envelope {
                request: Some(request),
                watch: None,
                message: Message::Watch {
                    source: source.to_owned(),
                    database: session_database(session),
                    presentation: terminal_presentation(),
                    refresh_floor: None,
                },
                extensions: BTreeMap::new(),
            },
        )
    }

    fn resync_over_socket(
        stream: &mut TcpStream,
        session: &LiveSession,
        request: [u8; 16],
        watch: [u8; 16],
    ) -> Envelope {
        request_over_socket(
            stream,
            session,
            Envelope {
                request: Some(request),
                watch: Some(watch),
                message: Message::Resync,
                extensions: BTreeMap::new(),
            },
        )
    }

    fn int_value(value: i64) -> CanonicalValue {
        CanonicalValue::new(OvbRaw::Int(value.into())).expect("canonical integer")
    }

    /// Asserts that `response` is a success result carrying `expected`.
    fn assert_success_value(response: &Envelope, expected: &CanonicalValue) {
        let Message::Result {
            status,
            value,
            diagnostic,
            ..
        } = &response.message
        else {
            panic!("Eval answers with a Result, not {response:?}");
        };
        assert_eq!(
            *status,
            orna_protocol_v1::ResultStatus::Success,
            "{diagnostic:?}"
        );
        assert_eq!(value.as_ref(), Some(expected));
    }

    fn assert_expression_result(response: &Envelope) {
        assert_success_value(response, &int_value(42));
    }

    #[test]
    fn cgi_environment_drops_git_proxy_and_token_variables() {
        let mut command = Command::new("/usr/bin/env");
        command.env("GIT_DIR", "/should/not/leak");
        command.env("GIT_CONFIG_GLOBAL", "/should/not/leak");
        command.env("HTTPS_PROXY", "http://should-not-leak.invalid");
        command.env("ORNA_SERVER_TOKEN", "should-not-leak");
        command.env("XDG_CONFIG_HOME", "/should/not/leak");
        clear_cgi_environment(&mut command);
        let output = command.output().expect("env runs");
        assert!(output.status.success());
        let names: Vec<String> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.split_once('=').map(|(name, _)| name.to_owned()))
            .collect();
        for name in &names {
            assert!(
                ["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR"].contains(&name.as_str()),
                "CGI child inherited unexpected variable {name}"
            );
        }
        for name in ["PATH", "HOME", "TMPDIR"] {
            if std::env::var_os(name).is_some() {
                assert!(names.iter().any(|kept| kept == name), "{name} was dropped");
            }
        }
    }
}
