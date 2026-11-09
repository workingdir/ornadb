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
use crate::encoding::percent_decode;
use crate::media_range::{MediaMethod, media_reply};
use orna_application::ApplicationLiveAdapter;
use orna_live::{
    LiveHost, LiveSessionAuthority, LiveTransport, SessionMetadata, SystemCredentialIssuer,
    TransportLimits, WebSocketApplicationPreparation, WebSocketOutput, WebSocketState,
    WebSocketUpgrade, WireRequest, WireResponse, encode_websocket_output,
};
use orna_protocol::{Envelope, Limits as ProtocolLimits, Message, PresentationContext};
use orna_repository::{GitCommitRef, ManagedPath, Repository, RuntimeGeneration};
use orna_security::{
    MAX_SESSION_LEASE, Origin, OriginPolicy, SessionBoundary, SessionDeletionAdapter,
};
use orna_serving::Serving;
use std::{
    collections::{BTreeMap, BTreeSet},
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
const SESSION_TERMINATION_TIMEOUT: StdDuration = StdDuration::from_mins(1);
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

/// How long one attached WebSocket has been silent, and whether the host has
/// pinged it for that silence: Ping once after `ping_after`, treat the peer as
/// gone after `idle_timeout`.
struct Silence {
    keepalive: WebSocketKeepalive,
    since: Instant,
    pinged: bool,
}

/// What the keepalive asks the worker to do before it reads again.
#[derive(Debug, Eq, PartialEq)]
enum SilenceStep {
    /// The peer stayed silent past the idle bound.
    Close,
    /// Silence passed the ping bound: send one Ping, then read for `wait`.
    Ping { wait: StdDuration },
    /// Read for `wait` before looking again.
    Wait(StdDuration),
}

impl Silence {
    fn new(keepalive: WebSocketKeepalive) -> Self {
        Self {
            keepalive,
            since: Instant::now(),
            pinged: false,
        }
    }

    /// The peer sent bytes: its silence, and any Ping owed for it, start over.
    fn heard(&mut self, now: Instant) {
        self.since = now;
        self.pinged = false;
    }

    /// A step served for the peer ended. Silence is measured from the later of
    /// its last bytes and this moment, because time spent evaluating its own
    /// requests, while nothing is read from it, is not silence.
    fn served(&mut self, now: Instant) {
        self.since = now;
    }

    fn next_step(&mut self, now: Instant) -> SilenceStep {
        let silent = now.saturating_duration_since(self.since);
        if silent >= self.keepalive.idle_timeout {
            return SilenceStep::Close;
        }
        let ping = !self.pinged && silent >= self.keepalive.ping_after;
        self.pinged |= ping;
        let bound = if self.pinged {
            self.keepalive.idle_timeout
        } else {
            self.keepalive.ping_after
        };
        let wait = bound
            .saturating_sub(silent)
            .max(StdDuration::from_millis(1));
        if ping {
            SilenceStep::Ping { wait }
        } else {
            SilenceStep::Wait(wait)
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

    fn origins(self) -> Result<Vec<Origin>, orna_security::BoundaryError> {
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
        // `HEAD` is a safe method: it carries the same headers as `GET` and no
        // body, so it is not a cross-origin write. A media payload is still not
        // readable by a foreign origin, so a foreign Origin on a media read is
        // refused as well.
        if matches!(request.method.as_str(), "GET" | "HEAD") && !is_websocket_route(request) {
            let foreign_media = (request.path.starts_with("/media/")
                || request.path.starts_with("/preview/"))
                && request_header(request, "origin")
                    .is_some_and(|origin| !is_same_origin(origin, host));
            return foreign_media.then(|| forbidden_response(request, "origin_denied"));
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
    app: Option<cli_serve_app::ServingEntry>,
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

/// Socket workers that own attached or attaching `WebSockets`, keyed by
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
    /// Set by [`WebSocketWorkers::cancel`] when the transport hands this
    /// attachment over for retirement while the worker is running. The worker
    /// stops reading frames and acknowledges the retirement as its last step.
    cancelled: Arc<AtomicBool>,
}

impl WebSocketWorkers {
    fn register(&self, attachment: [u8; 16], socket: &TcpStream) -> io::Result<Arc<AtomicBool>> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker = RegisteredWorker {
            socket: socket.try_clone()?,
            cancelled: Arc::clone(&cancelled),
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
        let workers = self.lock();
        let Some(worker) = workers.get(&attachment) else {
            return false;
        };
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
            .is_some_and(|worker| worker.cancelled.load(Ordering::Acquire));
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
    ) -> Result<SessionMetadata, orna_live::Error> {
        if database != self.identity.database_id {
            return Err(orna_live::Error::Denied);
        }
        let mut session = [0; 16];
        getrandom::fill(&mut session).map_err(|_| orna_live::Error::RuntimeUnavailable)?;
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
        .map_err(|_| orna_live::Error::RuntimeUnavailable)?;
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

    fn delete(&mut self, _: orna_security::SessionId) -> Result<(), Self::Error> {
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
    catalogue: orna_semantic::Catalogue,
    project: Option<orna_project::LoadedProject>,
    /// The admitted `serve` entry, or `None` when the project declares none.
    app: Option<cli_serve_app::ServingEntry>,
    /// The clone's durable runtime. The live host owns it, so each live
    /// request is durably admitted and an Eval's writes commit together with
    /// its terminal outcome (30-protocol REQUEST-1).
    runtime: RuntimeState,
    /// The factory for the served project's table-assertion validator, or
    /// `None` when the project declares no assertion. The factory runs once per
    /// commit against that commit's generation, so a false assertion refuses the
    /// write before any row is published (ORNA-ASSERT-042).
    table_candidate_validator: Option<orna_live::TableCandidateValidatorFactory>,
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
        .filter(|project| orna_conformance::project_declares_assertions(project))
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
    project: orna_project::LoadedProject,
) -> orna_live::TableCandidateValidatorFactory {
    Box::new(move |context: &orna_runtime::RuntimeActivationContext| {
        served_table_candidate_validator(&repository, &project, context)
    })
}

/// Builds the table-assertion validator for one staged commit. Any failure to
/// read the generation, its committed identities or the admitted project
/// fails the commit closed as `RuntimeUnavailable`, so the write is refused
/// before any row is published.
fn served_table_candidate_validator(
    repository: &Repository,
    project: &orna_project::LoadedProject,
    context: &orna_runtime::RuntimeActivationContext,
) -> orna_live::Result<Box<dyn orna_runtime::TableActivationCandidateValidator>> {
    let generation = u64::try_from(context.capture().generation())
        .map_err(|_| orna_live::Error::RuntimeUnavailable)?;
    let cwd_generation = repository
        .cwd_generation(RuntimeGeneration::new(generation))
        .map_err(|_| orna_live::Error::RuntimeUnavailable)?;
    let commit = cwd_generation
        .head()
        .ok_or(orna_live::Error::RuntimeUnavailable)?;
    let identities =
        orna_project::ObjectIdentityManifest::read_committed_if_present(repository, commit)
            .map_err(|_| orna_live::Error::RuntimeUnavailable)?
            .ok_or(orna_live::Error::RuntimeUnavailable)?;
    ensure_unambiguous_live_table_names(&identities)?;
    // The validator admits exactly the identities this activation admitted, so a
    // commit carrying them passes and a commit under any other identity is refused.
    // A name-addressed activation admits none.
    let table_object_ids = Arc::new(context.table_object_ids().clone());
    orna_conformance::transactional_loaded_project_candidate_validator(
        project,
        &identities,
        Some(cwd_generation),
        table_object_ids,
        Limits::default(),
    )
    .map_err(|_| orna_live::Error::RuntimeUnavailable)
}

/// Fails closed when two live table identities share one qualified name, so
/// the commit cannot pick one committed identity for that name. This host binds
/// every live table, so a duplicate under any name fails the commit.
fn ensure_unambiguous_live_table_names(
    identities: &orna_project::ObjectIdentityManifest,
) -> orna_live::Result<()> {
    let mut names = BTreeSet::new();
    for record in identities.records() {
        if record.kind() != orna_project::ObjectKind::Table || record.is_retired() {
            continue;
        }
        if !names.insert(record.qualified_name()) {
            return Err(orna_live::Error::RuntimeUnavailable);
        }
    }
    Ok(())
}

/// Loads and admits the serving entry. A broken `main.orna` is reported on
/// stderr and leaves the Git transport serving (ORNA-SERVE-007, ORNA-APP-020):
/// the project and entry are then absent and the catalogue is empty.
fn admit_serving_entry(
    endpoint: &Endpoint,
) -> (
    Option<orna_project::LoadedProject>,
    orna_semantic::Catalogue,
    Option<cli_serve_app::ServingEntry>,
) {
    let project = match load_project(endpoint) {
        Ok(project) => project,
        Err(diagnostic) => {
            report_serving_diagnostic(&diagnostic);
            return (None, orna_semantic::Catalogue::default(), None);
        }
    };
    let catalogue = match semantic_catalogue(&project) {
        Ok(catalogue) => catalogue,
        Err(diagnostic) => {
            report_serving_diagnostic(&diagnostic);
            return (None, orna_semantic::Catalogue::default(), None);
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
            let entry = app.map(|app| {
                cli_serve_app::ServingEntry::new(app, cli_serve_app::console_session(&project))
            });
            (Some(project), catalogue, entry)
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
        orna_live::Limits::default(),
        boundary,
        Serving::new(orna_serving::Limits::default()).map_err(|_| {
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
        app,
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
            Ok(0) | Err(_) => return false,
            Ok(_) => std::thread::sleep(StdDuration::from_micros(200)),
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
        if let Ok(socket) = stream.try_clone() {
            Some(Self {
                state: Arc::clone(state),
                socket,
            })
        } else {
            state.connections.fetch_sub(1, Ordering::AcqRel);
            None
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
fn close_code_for_failure(error: orna_live::Error) -> u16 {
    match error {
        orna_live::Error::Limit => 1009,
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
        let mut socket = WebSocketState::new(self.attachment);
        let mut stream = self.stream;
        let mut chunk = [0; WEBSOCKET_READ_CHUNK_BYTES];
        let mut input = initial;
        let mut silence = Silence::new(self.state.options.keepalive);
        loop {
            if !input.is_empty() {
                let bytes = std::mem::take(&mut input);
                if !self.serve_input(&mut socket, &bytes)? {
                    return Ok(());
                }
                silence.served(Instant::now());
            }
            if self.cancelled.load(Ordering::Acquire) {
                self.close(CLOSE_RETIRED);
                return Ok(());
            }
            let wait = match silence.next_step(Instant::now()) {
                SilenceStep::Close => {
                    self.close(CLOSE_IDLE);
                    return Ok(());
                }
                SilenceStep::Ping { wait } => {
                    self.write(&WebSocketOutput::Ping(Vec::new()))?;
                    wait
                }
                SilenceStep::Wait(wait) => wait,
            };
            stream.set_read_timeout(Some(wait))?;
            match stream.read(&mut chunk) {
                // Cancellation shuts down this socket's read half, which also
                // reads as end of stream; the loop then sends its Close frame.
                Ok(0) if self.cancelled.load(Ordering::Acquire) => {}
                Ok(0) => return Ok(()),
                Ok(read) => {
                    input.extend_from_slice(&chunk[..read]);
                    silence.heard(Instant::now());
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
                        Err(_) => ticket.reject(orna_live::Error::RuntimeUnavailable),
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
// repository path. Blob bytes are addressed by immutable commit id and path.
fn host_route(
    root: &Path,
    identity: RuntimeIdentity,
    allow_push: bool,
    request: &Request,
) -> Response {
    if let Some(response) = git_transport_route(root, allow_push, request) {
        return response;
    }
    if let Some(response) = git_content_route(root, request) {
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

const MAX_LISTING_FILE_BYTES: usize = 2 * 1024 * 1024;
fn git_content_route(root: &Path, request: &Request) -> Option<Response> {
    let suffix = request.path.strip_prefix("/content/")?;
    // The content route answers HEAD as well as GET; any other request falls
    // through to `host_route`, which answers `not_found`.
    if !matches!(request.method.as_str(), "GET" | "HEAD") {
        return None;
    }
    Some(content_route(root, request, suffix))
}

fn resolve_listing_commit(repository: &Repository, object_id: &str) -> Option<GitCommitRef> {
    if !matches!(object_id.len(), 40 | 64)
        || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    repository.resolve_snapshot(object_id).ok()
}

/// Serves a stored Blob's exact bytes with explicit byte-range support
/// (ORNA-APP-014). A `HEAD` carries the same headers and no body, an
/// unsatisfiable range answers `416`, and a payload that is not an allowlisted
/// media type is served as an attachment with `nosniff` and a sandbox policy so
/// a stored document cannot run script in the serving origin.
fn content_route(root: &Path, request: &Request, suffix: &str) -> Response {
    let Some((object_id, encoded_path)) = suffix.split_once('/') else {
        return not_found_response();
    };
    let Ok(repository) = Repository::discover(root) else {
        return unavailable_response();
    };
    let Some(commit) = resolve_listing_commit(&repository, object_id) else {
        return not_found_response();
    };
    let Some(decoded_path) = percent_decode(encoded_path, false) else {
        return bad_request_response();
    };
    let Ok(path) = ManagedPath::new(&decoded_path) else {
        return bad_request_response();
    };
    let Ok(bytes) = repository.read_committed_file(&commit, path.as_path(), MAX_LISTING_FILE_BYTES)
    else {
        return not_found_response();
    };
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let method = if request.method == "HEAD" {
        MediaMethod::Head
    } else {
        MediaMethod::Get
    };
    let range = request
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("range"))
        .map(|(_, value)| value.as_str());
    let reply = media_reply(method, range, length, |start, end| {
        let start = usize::try_from(start).map_err(|_| ())?;
        let end = usize::try_from(end).map_err(|_| ())?;
        bytes.get(start..=end).map(<[u8]>::to_vec).ok_or(())
    });
    let Ok(reply) = reply else {
        return bad_request_response();
    };
    let media_type = content_media_type(path.as_path());
    let mut response = Response::new(reply.plan.status, media_type, reply.body);
    response
        .headers
        .push(("accept-ranges".into(), "bytes".into()));
    response.headers.push((
        "content-length".into(),
        reply.plan.content_length.to_string(),
    ));
    if let Some(content_range) = &reply.plan.content_range {
        response
            .headers
            .push(("content-range".into(), content_range.clone()));
    }
    apply_payload_policy(&mut response, media_type);
    response
}

/// The media type served for a stored Blob path.
fn content_media_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("txt" | "orna") => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Media types a browser may render inline (the MIME-1 profile's raster image
/// and audio types). Exact essences only: `image/svg+xml` and `text/html` run
/// script when rendered in the serving origin, so they are never inline.
const INLINE_MEDIA_TYPES: [&str; 9] = [
    "audio/flac",
    "audio/mp4",
    "audio/mpeg",
    "audio/ogg",
    "audio/wav",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/webp",
];

/// True when `media_type`'s essence is an inline-renderable type. Parameters
/// after `;` do not change the essence, and an unlisted type is never inline.
fn is_allowlisted_media(media_type: &str) -> bool {
    let essence = media_type.split(';').next().unwrap_or_default().trim();
    INLINE_MEDIA_TYPES
        .iter()
        .any(|allowed| essence.eq_ignore_ascii_case(allowed))
}

/// Inserts `name: value` unless a header of that name (any case) is present,
/// so a policy header set by one layer is never duplicated by another.
fn ensure_header(response: &mut Response, name: &str, value: &str) {
    if !response
        .headers
        .iter()
        .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
    {
        response.headers.push((name.to_owned(), value.to_owned()));
    }
}

/// Applies the payload policy to a stored Blob response. An allowlisted type
/// keeps its media type and renders inline. Any other payload is served as an
/// `application/octet-stream` attachment with `nosniff` and a `sandbox` policy,
/// so a stored document cannot run script in the serving origin.
fn apply_payload_policy(response: &mut Response, media_type: &str) {
    if is_allowlisted_media(media_type) {
        media_type.clone_into(&mut response.content_type);
        return;
    }
    "application/octet-stream".clone_into(&mut response.content_type);
    ensure_header(response, "content-disposition", "attachment");
    ensure_header(response, "x-content-type-options", "nosniff");
    ensure_header(response, "content-security-policy", "sandbox");
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
    if let Some(protocol) = request_header(request, "git-protocol")
        && protocol == "version=2"
    {
        command.env("GIT_PROTOCOL", protocol);
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
/// exec path itself (compiled-in or `RUNTIME_PREFIX`), so `GIT_EXEC_PATH` is not
/// forwarded; `XDG_CONFIG_HOME` is not forwarded, so only ~/.gitconfig applies.
/// Requires git >= 2.30 (`commit_optional` relies on `rev-parse --end-of-options`).
fn clear_cgi_environment(command: &mut Command) {
    command.env_clear();
    for name in ["PATH", "HOME", "LANG", "LC_ALL", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

#[allow(clippy::assigning_clones)]
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
        || !matches!(method.as_str(), "GET" | "HEAD" | "POST" | "DELETE")
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

#[allow(clippy::needless_pass_by_value)]
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

#[allow(clippy::needless_pass_by_value)]
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
    let declares = |name: &str| {
        response
            .headers
            .iter()
            .any(|(existing, _)| existing.eq_ignore_ascii_case(name))
    };
    if response.content_type.starts_with("text/html") {
        for (name, value) in HTML_SECURITY_HEADERS {
            if !declares(name) {
                let _ = write!(head, "{name}: {value}\r\n");
            }
        }
    }
    for (name, value) in &response.headers {
        let _ = write!(head, "{name}: {value}\r\n");
    }
    // A response that declares its own Content-Length (a media range) keeps it;
    // the body length is the default only.
    if !declares("content-length") {
        let _ = write!(head, "Content-Length: {}\r\n", response.body.len());
    }
    head.push_str("Connection: close\r\n\r\n");
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

#[allow(clippy::items_after_statements)]
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

#[allow(clippy::items_after_statements)]
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
    } else if path.starts_with(cli_serve_app::ACTION_PREFIX) {
        cli_serve_app::ACTION_REQUEST_BODY_BYTES
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
mod tests;
