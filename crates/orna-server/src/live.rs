//! Loopback live-session host.
//!
//! This is the first executable-owned live boundary. Its basic entry point
//! accepts one local HTTP connection; the cancellable entry point retains
//! transport/session state across concurrent connections and hands the live
//! WebSocket path to the bounded transport driver. TLS, remote exposure,
//! durable session credentials, and application dispatch are deliberately
//! outside this slice. Verified runtime request reservations and terminal
//! outcomes are retained through the existing runtime state boundary.

use crate::live_eval::{PureEvalApplication, SessionExpiries};
use futures::{
    Future, FutureExt, StreamExt,
    executor::block_on,
    io::{AsyncReadExt, AsyncWriteExt},
};
use orna_live_v1::{
    HttpConnection, HttpConnectionError, HttpIoError, Limits, LiveApplicationCompletion,
    LiveApplicationTicket, LiveApplicationWorkSupervisor, LiveHost, LiveSessionAuthority,
    LiveSessionChildren, LiveTransport, SessionMetadata, SystemCredentialIssuer, TransportLimits,
    WebSocketApplicationPreparation, WebSocketOutput, WebSocketState, encode_websocket_output,
    parse_http_request,
};
use orna_project_v1::ProjectLoader;
use orna_protocol_v1::{Envelope, Limits as ProtocolLimits, Message, PresentationContext};
use orna_repository_v1::{Repository, inspect_metadata};
use orna_runtime_v1::{RequestIdentity, RuntimeIdentity, RuntimeState};
use orna_security_v1::{Origin, OriginPolicy, SessionBoundary, SessionDeletionAdapter, SessionId};
use orna_serving_v1::{Limits as ServingLimits, Serving};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fmt, io,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll},
    time::Duration,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

/// Stable failures from the executable-owned live boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiveHostError {
    Repository,
    Runtime,
    Configuration,
    Listener,
    Connection,
    Cancelled,
}

impl fmt::Display for LiveHostError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Repository => "orna: live repository authority unavailable",
            Self::Runtime => "orna: live runtime authority unavailable",
            Self::Configuration => "orna: live host configuration unavailable",
            Self::Listener => "orna: live loopback listener unavailable",
            Self::Connection => "orna: live HTTP connection failed",
            Self::Cancelled => "orna: live host cancelled",
        })
    }
}

impl std::error::Error for LiveHostError {}

/// One loopback-only live host. The listener and session state are owned by
/// this value until [`Self::serve`] completes one HTTP connection.
pub struct LiveOnceHost {
    listener: orna_live_v1::LiveListener,
    transport: LiveTransport,
    authority: HostAuthority,
    deletion: HostDeletion,
    application: SharedApplication,
    application_work: LiveApplicationWorkSupervisor,
    application_recipe: ApplicationWorkerRecipe,
}

type SharedApplication = Rc<RefCell<Option<PureEvalApplication>>>;
type DeletedLeaseIndex = Rc<RefCell<BTreeMap<SessionId, u64>>>;

#[derive(Clone)]
struct ApplicationWorkerRecipe {
    repository: Repository,
    project: orna_project_v1::LoadedProject,
    capture: orna_foundation_v1::CwdCapture,
    database_id: [u8; 16],
    identity: RuntimeIdentity,
    initial_digest: [u8; 32],
    runtime_owner: [u8; 16],
    #[cfg(test)]
    eval_started: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    #[cfg(test)]
    panic_on_eval: bool,
}

struct ApplicationWorkerRegistry {
    recipe: ApplicationWorkerRecipe,
    workers: Rc<RefCell<BTreeMap<SessionId, ApplicationWorkerHandle>>>,
}

struct ApplicationWorkerHandle {
    sender: std::sync::mpsc::Sender<ApplicationWorkerCommand>,
    join: Option<std::thread::JoinHandle<()>>,
}

enum ApplicationWorkerCommand {
    Execute(ApplicationJob),
    Shutdown,
}

struct ApplicationJob {
    socket: Option<WebSocketState>,
    ticket: Option<LiveApplicationTicket>,
    reply: Option<
        futures::channel::oneshot::Sender<
            Result<(WebSocketState, Vec<WebSocketOutput>), orna_live_v1::Error>,
        >,
    >,
    completion_sender: futures::channel::mpsc::UnboundedSender<ActorCommand>,
}

// A deliberately smaller executable cap than the transport's retained-session
// bound. A session consumes one evaluator OS thread, so this is checked before
// spawn. Cancellation is a shared atomic flag consumed directly by the
// evaluator token; it does not require a second polling thread.
const MAX_EVALUATOR_WORKERS: usize = 64;

impl ApplicationWorkerRegistry {
    fn new(recipe: ApplicationWorkerRecipe) -> Self {
        Self {
            recipe,
            workers: Rc::new(RefCell::new(BTreeMap::new())),
        }
    }

    fn submit(
        &self,
        session: [u8; 16],
        job: ApplicationJob,
    ) -> std::result::Result<(), ApplicationJob> {
        let session = SessionId::new(session);
        if !self.workers.borrow().contains_key(&session) {
            if self.workers.borrow().len() >= MAX_EVALUATOR_WORKERS {
                return Err(job);
            }
            let (sender, receiver) = std::sync::mpsc::channel();
            let recipe = self.recipe.clone();
            let join = std::thread::Builder::new()
                .name("orna-live-evaluator".into())
                .spawn(move || application_worker_loop(recipe, receiver))
                .map_err(|_| ())
                .ok();
            let Some(join) = join else {
                return Err(job);
            };
            self.workers.borrow_mut().insert(
                session,
                ApplicationWorkerHandle {
                    sender,
                    join: Some(join),
                },
            );
        }
        let result = {
            let workers = self.workers.borrow();
            let worker = workers.get(&session).expect("worker was inserted");
            worker.sender.send(ApplicationWorkerCommand::Execute(job))
        };
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                // A worker which has exited (including after an evaluator
                // panic) must not remain as a dead registry entry. Remove and
                // join it before returning the unadmitted job so a later
                // request can establish a fresh, bounded owner.
                let handle = self.workers.borrow_mut().remove(&session);
                if let Some(mut handle) = handle
                    && let Some(join) = handle.join.take()
                {
                    let _ = join.join();
                }
                match error.0 {
                    ApplicationWorkerCommand::Execute(job) => Err(job),
                    ApplicationWorkerCommand::Shutdown => unreachable!(),
                }
            }
        }
    }

    fn stop_and_join(
        &self,
        session: SessionId,
    ) -> Pin<Box<dyn Future<Output = orna_live_v1::Result<()>>>> {
        let handle = self
            .workers
            .borrow_mut()
            .remove(&session)
            .map(|mut handle| {
                let _ = handle.sender.send(ApplicationWorkerCommand::Shutdown);
                handle.join.take().expect("worker join handle retained")
            });
        Box::pin(async move {
            let Some(handle) = handle else {
                return Ok(());
            };
            tokio::task::spawn_blocking(move || handle.join().map_err(|_| ()))
                .await
                .map_err(|_| orna_live_v1::Error::DeletionFailed)?
                .map_err(|_| orna_live_v1::Error::DeletionFailed)
        })
    }

    fn stop_and_join_blocking(&self, session: SessionId) -> std::result::Result<(), ()> {
        let handle = self
            .workers
            .borrow_mut()
            .remove(&session)
            .map(|mut handle| {
                let _ = handle.sender.send(ApplicationWorkerCommand::Shutdown);
                handle.join.take().expect("worker join handle retained")
            });
        handle.map_or(Ok(()), |handle| handle.join().map_err(|_| ()))
    }

    fn stop_all(&self) -> Pin<Box<dyn Future<Output = orna_live_v1::Result<()>>>> {
        let sessions = self.workers.borrow().keys().copied().collect::<Vec<_>>();
        let registry = self.clone();
        Box::pin(async move {
            for session in sessions {
                registry.stop_and_join(session).await?;
            }
            Ok(())
        })
    }
}

impl Clone for ApplicationWorkerRegistry {
    fn clone(&self) -> Self {
        Self {
            recipe: self.recipe.clone(),
            workers: Rc::clone(&self.workers),
        }
    }
}

impl Drop for ApplicationWorkerRegistry {
    fn drop(&mut self) {
        if Rc::strong_count(&self.workers) != 1 {
            return;
        }
        let handles = std::mem::take(&mut *self.workers.borrow_mut());
        for (_, mut handle) in handles {
            let _ = handle.sender.send(ApplicationWorkerCommand::Shutdown);
            if let Some(join) = handle.join.take() {
                let _ = join.join();
            }
        }
    }
}

fn application_worker_loop(
    recipe: ApplicationWorkerRecipe,
    receiver: std::sync::mpsc::Receiver<ApplicationWorkerCommand>,
) {
    let expiries = Rc::new(RefCell::new(BTreeMap::new()));
    #[cfg(test)]
    let eval_started = recipe.eval_started.clone();
    #[cfg(test)]
    let panic_on_eval = recipe.panic_on_eval;
    let mut application = match PureEvalApplication::from_repository_with_project(
        &recipe.repository,
        recipe.database_id,
        recipe.identity,
        recipe.initial_digest,
        recipe.runtime_owner,
        Rc::clone(&expiries),
        Some(recipe.project),
        Some(recipe.capture.clone()),
    ) {
        Ok(application) => application,
        Err(()) => {
            for command in receiver {
                if let ApplicationWorkerCommand::Execute(mut job) = command {
                    job.reject(orna_live_v1::Error::ApplicationRejected);
                }
            }
            return;
        }
    };
    #[cfg(test)]
    application.set_test_controls(eval_started, panic_on_eval);
    while let Ok(command) = receiver.recv() {
        match command {
            ApplicationWorkerCommand::Shutdown => break,
            ApplicationWorkerCommand::Execute(mut job) => {
                let Some(ticket) = job.ticket.take() else {
                    continue;
                };
                let completion = if ticket.is_cancelled() {
                    ticket.reject(orna_live_v1::Error::Closed)
                } else {
                    application.activate_worker_session(ticket.session());
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        futures::executor::block_on(ticket.execute(&mut application))
                    }));
                    match result {
                        Ok(completion) => completion,
                        Err(_) => {
                            // The ticket's lease is dropped while unwinding,
                            // so the supervisor records a failed child. The
                            // socket reply still needs a stable terminal
                            // result; otherwise a panic strands the caller
                            // even though the worker is about to exit.
                            job.fail(orna_live_v1::Error::ApplicationRejected);
                            // The application owner is no longer trusted after
                            // a panic; queued jobs are rejected by ApplicationJob
                            // drops as the receiver is abandoned.
                            return;
                        }
                    }
                };
                job.finish(completion);
            }
        }
    }
}

impl ApplicationJob {
    fn finish(&mut self, completion: LiveApplicationCompletion) {
        let Some(socket) = self.socket.take() else {
            return;
        };
        let Some(reply) = self.reply.take() else {
            return;
        };
        let _ = self
            .completion_sender
            .unbounded_send(ActorCommand::ApplicationComplete {
                socket,
                completion,
                reply,
            });
    }

    fn reject(&mut self, error: orna_live_v1::Error) {
        let Some(ticket) = self.ticket.take() else {
            return;
        };
        self.finish(ticket.reject(error));
    }

    fn fail(&mut self, error: orna_live_v1::Error) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(Err(error));
        }
        self.socket.take();
    }
}

impl Drop for ApplicationJob {
    fn drop(&mut self) {
        if self.ticket.is_some() {
            self.reject(orna_live_v1::Error::ApplicationRejected);
        }
    }
}

impl LiveOnceHost {
    /// Binds a one-shot host to the default loopback listener.
    pub fn bind(repository: &Repository, port: u16) -> Result<Self, LiveHostError> {
        let metadata = inspect_metadata(repository)
            .map_err(|_| LiveHostError::Repository)?
            .ok_or(LiveHostError::Repository)?;
        let database_id = *metadata.database_id().as_bytes();
        let (identity, initial_digest) = runtime_identity(database_id);
        let state = block_on(RuntimeState::open(repository, identity, initial_digest))
            .map_err(|_| LiveHostError::Runtime)?;
        let persisted = block_on(state.identity()).map_err(|_| LiveHostError::Runtime)?;
        if persisted.database_id != database_id || persisted != identity {
            return Err(LiveHostError::Runtime);
        }
        let capture = block_on(state.capture()).map_err(|_| LiveHostError::Runtime)?;
        if capture.database_id() != database_id {
            return Err(LiveHostError::Runtime);
        }
        let runtime_id = capture.runtime_id();
        let project = ProjectLoader::default()
            .load_with_standard_profile(
                repository,
                Some(orna_evaluator_v1::reference_standard_profile()),
            )
            .map_err(|_| LiveHostError::Repository)?;
        let expiries: SessionExpiries = Rc::new(RefCell::new(BTreeMap::new()));
        let deleted_leases: DeletedLeaseIndex = Rc::new(RefCell::new(BTreeMap::new()));
        let mut runtime_owner = [0; 16];
        getrandom::fill(&mut runtime_owner).map_err(|_| LiveHostError::Runtime)?;
        if runtime_owner == [0; 16] {
            return Err(LiveHostError::Runtime);
        }
        let application = Rc::new(RefCell::new(Some(
            PureEvalApplication::from_repository(
                repository,
                database_id,
                identity,
                initial_digest,
                runtime_owner,
                Rc::clone(&expiries),
            )
            .map_err(|_| LiveHostError::Repository)?,
        )));
        let application_recipe = ApplicationWorkerRecipe {
            repository: repository.clone(),
            project,
            capture: capture.clone(),
            database_id,
            identity,
            initial_digest,
            runtime_owner,
            #[cfg(test)]
            eval_started: None,
            #[cfg(test)]
            panic_on_eval: false,
        };
        let retained_capture = block_on(state.capture()).map_err(|_| LiveHostError::Runtime)?;
        if retained_capture != capture {
            return Err(LiveHostError::Runtime);
        }

        let listener =
            LiveTransport::bind_default_listener(port).map_err(|_| LiveHostError::Listener)?;
        let address = listener.status().address;
        let localhost = Origin::parse(format!("http://localhost:{}", address.port()))
            .map_err(|_| LiveHostError::Configuration)?;
        let loopback = Origin::parse(format!("http://127.0.0.1:{}", address.port()))
            .map_err(|_| LiveHostError::Configuration)?;
        let bare_localhost =
            Origin::parse("http://localhost").map_err(|_| LiveHostError::Configuration)?;
        let host = LiveHost::with_runtime_state_and_owner(
            Limits::default(),
            SessionBoundary::new(
                OriginPolicy::new([bare_localhost, localhost, loopback], []),
                30_000,
            ),
            Serving::new(ServingLimits::default()).map_err(|_| LiveHostError::Configuration)?,
            state,
            runtime_owner,
        )
        .map_err(|_| LiveHostError::Configuration)?;
        let transport = LiveTransport::new(host, TransportLimits::default())
            .map_err(|_| LiveHostError::Configuration)?;
        let application_work = transport.application_work_supervisor();
        Ok(Self {
            listener,
            transport,
            authority: HostAuthority {
                database_id,
                runtime_id,
                expiries: Rc::clone(&expiries),
            },
            deletion: HostDeletion {
                expiries,
                deleted_leases,
                application: Rc::clone(&application),
                application_workers: None,
            },
            application,
            application_work,
            application_recipe,
        })
    }

    /// Returns the bound loopback address before the host accepts its peer.
    #[must_use]
    pub const fn address(&self) -> std::net::SocketAddr {
        self.listener.status().address
    }

    /// Accepts and serves exactly one bounded HTTP session-create connection.
    pub fn serve(self) -> Result<(), LiveHostError> {
        self.serve_with_cancellation(futures::future::pending())
    }

    /// Serves one loopback connection while racing accept and connection I/O
    /// against the caller-owned cancellation future. The host owns the
    /// listener and accepted socket for the complete task lifetime.
    pub fn serve_with_cancellation<C>(mut self, mut cancellation: C) -> Result<(), LiveHostError>
    where
        C: Future<Output = ()> + Unpin,
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|_| LiveHostError::Configuration)?;
        runtime.block_on(self.serve_with_cancellation_async(&mut cancellation))
    }

    /// Accepts concurrent loopback connections until the caller cancels.
    /// Transport/session state remains owned by this host across connections.
    pub fn serve_until_cancellation<C>(self, mut cancellation: C) -> Result<(), LiveHostError>
    where
        C: Future<Output = ()> + Unpin,
    {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|_| LiveHostError::Configuration)?;
        let local = tokio::task::LocalSet::new();
        local.block_on(
            &runtime,
            self.serve_concurrently_with_cancellation(&mut cancellation),
        )
    }

    async fn serve_concurrently_with_cancellation<C>(
        self,
        cancellation: &mut C,
    ) -> Result<(), LiveHostError>
    where
        C: Future<Output = ()> + Unpin,
    {
        let LiveOnceHost {
            listener,
            transport,
            authority,
            mut deletion,
            application: _,
            application_work,
            application_recipe,
        } = self;
        let listener = listener
            .listener()
            .try_clone()
            .map_err(|_| LiveHostError::Listener)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| LiveHostError::Listener)?;
        let listener =
            tokio::net::TcpListener::from_std(listener).map_err(|_| LiveHostError::Listener)?;
        let registry = Rc::new(RefCell::new(WorkerRegistry::default()));
        let (actor_sender, actor_receiver) = futures::channel::mpsc::unbounded();
        let (retirement_ack_sender, retirement_ack_receiver) = futures::channel::mpsc::unbounded();
        let (retirement_gate_sender, mut retirement_gate_receiver) =
            futures::channel::mpsc::unbounded();
        let application_workers = ApplicationWorkerRegistry::new(application_recipe);
        deletion.application_workers = Some(application_workers.clone());
        let actor = tokio::task::spawn_local(run_host_actor(
            actor_receiver,
            actor_sender.clone(),
            ConcurrentHostState {
                transport,
                authority,
                issuer: SystemCredentialIssuer::default(),
                deletion,
                application_workers,
                application_work,
                admitted_upgrades: BTreeMap::new(),
                delivering_upgrades: BTreeMap::new(),
            },
            Rc::clone(&registry),
            retirement_ack_receiver,
            retirement_gate_sender,
        ));
        let mut workers = tokio::task::JoinSet::new();
        // Retirements created by an actor-owned event (such as expiry) have
        // no connection worker waiting for them. Keep their join gates in the
        // host supervisor so shutdown drains successful acknowledgements while
        // the actor still owns the transport fence.
        let mut retirement_tasks = tokio::task::JoinSet::new();
        let mut actor = Some(actor);
        loop {
            tokio::select! {
                biased;
                () = &mut *cancellation => {
                    drop(listener);
                    return shutdown_concurrent_host(
                        &registry,
                        &mut workers,
                        &mut retirement_tasks,
                        &mut retirement_gate_receiver,
                        &retirement_ack_sender,
                        actor_sender,
                        actor.take(),
                        LiveHostError::Cancelled,
                    ).await;
                }
                result = actor.as_mut().expect("actor remains owned until shutdown") => {
                    // Polling the JoinHandle above has already joined this actor.
                    // Drop the resolved handle rather than polling it a second time.
                    let _ = actor.take();
                    let _ = result;
                    drop(listener);
                    return shutdown_concurrent_host(
                        &registry,
                        &mut workers,
                        &mut retirement_tasks,
                        &mut retirement_gate_receiver,
                        &retirement_ack_sender,
                        actor_sender,
                        None,
                        LiveHostError::Runtime,
                    ).await;
                }
                result = workers.join_next_with_id(), if !workers.is_empty() => {
                    if result.is_some_and(|result| acknowledge_worker_join(&registry, result)) {
                        drop(listener);
                        return shutdown_concurrent_host(
                            &registry,
                            &mut workers,
                            &mut retirement_tasks,
                            &mut retirement_gate_receiver,
                            &retirement_ack_sender,
                            actor_sender,
                            actor.take(),
                            LiveHostError::Connection,
                        ).await;
                    }
                }
                retirement = retirement_gate_receiver.next() => {
                    let Some(retirement) = retirement else {
                        drop(listener);
                        return shutdown_concurrent_host(
                            &registry,
                            &mut workers,
                            &mut retirement_tasks,
                            &mut retirement_gate_receiver,
                            &retirement_ack_sender,
                            actor_sender,
                            actor.take(),
                            LiveHostError::Runtime,
                        ).await;
                    };
                    retirement_tasks.spawn_local(supervise_retirement_gates(
                        retirement_ack_sender.clone(),
                        retirement,
                    ));
                }
                result = retirement_tasks.join_next(), if !retirement_tasks.is_empty() => {
                    // Failed or dropped worker joins deliberately leave their
                    // identities fenced; the task itself has no host error.
                    let _ = result;
                }
                result = listener.accept() => {
                    let (stream, _) = match result {
                        Ok(stream) => stream,
                        Err(_) => {
                            drop(listener);
                            return shutdown_concurrent_host(
                                &registry,
                                &mut workers,
                                &mut retirement_tasks,
                                &mut retirement_gate_receiver,
                                &retirement_ack_sender,
                                actor_sender,
                                actor.take(),
                                LiveHostError::Connection,
                            ).await;
                        }
                    };
                    let (worker_id, cancellation_receiver) = registry.borrow_mut().register();
                    let actor = actor_sender.clone();
                    let retirement = retirement_ack_sender.clone();
                    let worker_registry = Rc::clone(&registry);
                    let task = workers.spawn_local(async move {
                        serve_socket_worker(
                            stream,
                            actor.clone(),
                            retirement,
                            worker_registry,
                            worker_id,
                            cancellation_receiver,
                        )
                        .await;
                        actor_worker_exited(&actor, worker_id).await;
                        worker_id
                    });
                    registry.borrow_mut().bind_task(worker_id, task.id());
                }
            }
        }
    }

    async fn serve_with_cancellation_async<C>(
        &mut self,
        cancellation: &mut C,
    ) -> Result<(), LiveHostError>
    where
        C: Future<Output = ()> + Unpin,
    {
        let listener = self
            .listener
            .listener()
            .try_clone()
            .map_err(|_| LiveHostError::Listener)?;
        listener
            .set_nonblocking(true)
            .map_err(|_| LiveHostError::Listener)?;
        let listener =
            tokio::net::TcpListener::from_std(listener).map_err(|_| LiveHostError::Listener)?;
        let (stream, _) = tokio::select! {
            result = listener.accept() => result.map_err(|_| LiveHostError::Connection)?,
            () = &mut *cancellation => return Err(LiveHostError::Cancelled),
        };
        let (reader, writer) = stream.into_split();
        let reader = TokioReader(reader);
        let mut reader = PrefixedReader::new(reader);
        let mut writer = TokioWriter(writer);
        let initial = read_initial_request(&mut reader, cancellation).await?;
        let websocket = parse_http_request(&initial, TransportLimits::default())
            .map_err(|_| LiveHostError::Connection)?
            .is_some_and(|request| {
                request.request().method == "GET"
                    && request.request().path.starts_with("/orna/live/")
            });
        reader.replay(initial);
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut issuer = SystemCredentialIssuer::default();
        let mut clock = system_milliseconds;
        if websocket {
            let attachment = opaque_attachment().map_err(|_| LiveHostError::Configuration)?;
            let application = Rc::clone(&self.application);
            let mut application = application
                .borrow_mut()
                .take()
                .ok_or(LiveHostError::Connection)?;
            application.expire(system_milliseconds());
            let outcome = self
                .transport
                .serve_websocket_connection(
                    &mut reader,
                    &mut writer,
                    &mut connection,
                    attachment,
                    &mut clock,
                    cancellation,
                    &mut application,
                )
                .await;
            self.application.borrow_mut().replace(application);
            outcome.map_err(map_connection_error)
        } else {
            self.serve_http_connection_with_children(
                &mut reader,
                &mut writer,
                &mut connection,
                &mut clock,
                cancellation,
                &mut issuer,
            )
            .await
        }
    }

    /// Runs the one-connection HTTP boundary through the same child-aware
    /// DELETE route as the concurrent host. The generic transport stream
    /// helpers deliberately remain child-agnostic for embedders that have no
    /// application callbacks; this executable adapter cannot use them for a
    /// request that may delete a session.
    async fn serve_http_connection_with_children<C>(
        &mut self,
        reader: &mut (impl futures::io::AsyncRead + Unpin),
        writer: &mut (impl futures::io::AsyncWrite + Unpin),
        connection: &mut HttpConnection,
        clock: &mut C,
        cancellation: &mut (impl Future<Output = ()> + Unpin),
        issuer: &mut SystemCredentialIssuer,
    ) -> Result<(), LiveHostError>
    where
        C: FnMut() -> u64,
    {
        let mut chunk = [0; 8192];
        loop {
            let count = await_host_io(reader.read(&mut chunk), cancellation).await?;
            if count == 0 {
                return if connection.buffered_bytes() == 0 {
                    Ok(())
                } else {
                    Err(LiveHostError::Connection)
                };
            }
            let requests = connection
                .push(&chunk[..count])
                .map_err(|_| LiveHostError::Connection)?;
            for request in requests {
                let now = clock();
                let request = request.request().clone();
                let response = if self.deletion.expired_deleted_lease(&request, now) {
                    expired_delete_response()
                } else {
                    let mut children = HostApplicationChildren::new(self.application_work.clone());
                    self.transport
                        .handle_with_children(
                            request,
                            now,
                            &mut self.authority,
                            issuer,
                            &mut self.deletion,
                            &mut children,
                        )
                        .await
                }
                .encode_http(TransportLimits::default())
                .map_err(|_| LiveHostError::Connection)?;
                await_host_io(writer.write_all(&response), cancellation).await?;
                await_host_io(writer.flush(), cancellation).await?;
            }
        }
    }
}

struct ConcurrentHostState {
    transport: LiveTransport,
    authority: HostAuthority,
    issuer: SystemCredentialIssuer,
    deletion: HostDeletion,
    application_workers: ApplicationWorkerRegistry,
    application_work: LiveApplicationWorkSupervisor,
    // A successful Begin is actor-owned before the worker enters the
    // delivery barrier. Retain that exact reservation so worker exit can
    // serialize an abort even when no provisional 101 was attempted.
    admitted_upgrades: BTreeMap<u64, AdmittedUpgrade>,
    // This is the executable half of the handshake linearization barrier.
    // It pairs the worker that may write a provisional 101 with the exact
    // transport reservation until commit or serialized abort.
    delivering_upgrades: BTreeMap<u64, DeliveryStart>,
}

struct DeliveryStart {
    upgrade: orna_live_v1::WebSocketUpgrade,
    accepted_wall_start: u64,
    accepted_instant: Instant,
}

struct AdmittedUpgrade {
    upgrade: orna_live_v1::WebSocketUpgrade,
    begun_wall_start: u64,
}

fn schedule_application(
    state: &mut ConcurrentHostState,
    pending: PendingApplication,
    actor_sender: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
) {
    let session = pending.ticket.session();
    let job = ApplicationJob {
        socket: Some(pending.socket),
        ticket: Some(pending.ticket),
        reply: Some(pending.reply),
        completion_sender: actor_sender.clone(),
    };
    if let Err(mut job) = state.application_workers.submit(session, job) {
        job.reject(orna_live_v1::Error::Limit);
    }
}

enum ActorCommand {
    Http {
        connection: HttpConnection,
        bytes: Vec<u8>,
        reply: futures::channel::oneshot::Sender<Result<ActorHttpResult, HttpConnectionError>>,
    },
    Begin {
        request: orna_live_v1::WireRequest,
        attachment: [u8; 16],
        worker_id: u64,
        reply: futures::channel::oneshot::Sender<
            Result<orna_live_v1::WebSocketUpgrade, orna_live_v1::WireResponse>,
        >,
    },
    Deliver {
        upgrade: orna_live_v1::WebSocketUpgrade,
        worker_id: u64,
        reply: futures::channel::oneshot::Sender<Result<(), ()>>,
    },
    Commit {
        upgrade: orna_live_v1::WebSocketUpgrade,
        worker_id: u64,
        delivered_at_instant: Instant,
        reply: futures::channel::oneshot::Sender<
            Result<(orna_live_v1::WireResponse, RetirementGates), ()>,
        >,
    },
    Abort {
        upgrade: orna_live_v1::WebSocketUpgrade,
        worker_id: u64,
        reply: futures::channel::oneshot::Sender<()>,
    },
    WorkerExited {
        worker_id: u64,
        reply: futures::channel::oneshot::Sender<()>,
    },
    Receive {
        socket: WebSocketState,
        bytes: Vec<u8>,
        now: u64,
        reply: futures::channel::oneshot::Sender<
            Result<(WebSocketState, Vec<WebSocketOutput>), orna_live_v1::Error>,
        >,
    },
    ApplicationComplete {
        socket: WebSocketState,
        completion: LiveApplicationCompletion,
        reply: futures::channel::oneshot::Sender<
            Result<(WebSocketState, Vec<WebSocketOutput>), orna_live_v1::Error>,
        >,
    },
    /// Stops admitted application work without relinquishing the actor's
    /// responsibility to process socket-worker retirement acknowledgements.
    DrainApplicationWork,
    Shutdown,
    Close {
        attachment: [u8; 16],
        now: u64,
        reply: futures::channel::oneshot::Sender<
            Result<orna_live_v1::FrameOutcome, orna_live_v1::Error>,
        >,
    },
}

struct PendingApplication {
    socket: WebSocketState,
    ticket: LiveApplicationTicket,
    reply: futures::channel::oneshot::Sender<
        Result<(WebSocketState, Vec<WebSocketOutput>), orna_live_v1::Error>,
    >,
}

struct ActorHttpResult {
    connection: HttpConnection,
    responses: Vec<Vec<u8>>,
    retirement: RetirementGates,
}

struct RetirementGate {
    attachment: [u8; 16],
    completion: futures::channel::oneshot::Receiver<Result<(), ()>>,
}

type RetirementGates = Vec<RetirementGate>;

enum RetirementAcknowledgement {
    Acknowledge {
        attachment: [u8; 16],
        reply: futures::channel::oneshot::Sender<bool>,
    },
}

async fn run_host_actor(
    mut commands: futures::channel::mpsc::UnboundedReceiver<ActorCommand>,
    actor_sender: futures::channel::mpsc::UnboundedSender<ActorCommand>,
    mut state: ConcurrentHostState,
    registry: Rc<RefCell<WorkerRegistry>>,
    mut retirement_acknowledgements: futures::channel::mpsc::UnboundedReceiver<
        RetirementAcknowledgement,
    >,
    retirement_gates: futures::channel::mpsc::UnboundedSender<RetirementGates>,
) {
    use futures::StreamExt;
    let mut ticker = tokio::time::interval(Duration::from_millis(100));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut retirement_acknowledgements_open = true;
    let mut application_drain = None;
    loop {
        let command = tokio::select! {
            command = commands.next() => match command {
                Some(command) => command,
                None => break,
            },
            _ = ticker.tick() => {
                let now = system_milliseconds();
                // The deadline revokes only the worker's right to keep
                // delivering. Its reservation remains protected until that
                // worker returns through Abort and the supervisor joins it.
                let overdue = state
                    .delivering_upgrades
                    .iter()
                    .filter_map(|(worker, delivery)| {
                        (now >= delivery.upgrade.deadline()).then_some(*worker)
                    })
                    .collect::<Vec<_>>();
                for worker in overdue {
                    registry.borrow_mut().cancel(worker);
                }
                let mut children = HostApplicationChildren::with_workers(
                    state.application_work.clone(),
                    state.application_workers.clone(),
                );
                let _ = state
                    .transport
                    .expire_sessions_with_children(now, &mut state.deletion, &mut children)
                    .await;
                let Ok(retirement) = capture_retirement_gates(
                    &registry,
                    state.transport.take_retired_attachments(),
                ) else {
                    return;
                };
                if retirement_gates.unbounded_send(retirement).is_err() {
                    return;
                }
                continue;
            }
            acknowledgement = retirement_acknowledgements.next(), if retirement_acknowledgements_open => {
                match acknowledgement {
                    Some(RetirementAcknowledgement::Acknowledge { attachment, reply }) => {
                        let _ = reply.send(state.transport.acknowledge_retired_attachment(attachment));
                    }
                    None => retirement_acknowledgements_open = false,
                }
                continue;
            }
        };
        match command {
            ActorCommand::DrainApplicationWork => {
                // Socket workers can still be awaiting WorkerExited while
                // their application work drains. This command is deliberately
                // nonterminal so their actor-owned retirement cleanup remains
                // available until the outer supervisor has joined them.
                if application_drain.is_none() {
                    let application_work = state.application_work.clone();
                    application_drain = Some(tokio::task::spawn_local(async move {
                        application_work.cancel_and_join_all().await
                    }));
                }
            }
            ActorCommand::Shutdown => {
                // The socket supervisor sends terminal shutdown only after
                // every WorkerExited acknowledgement and retirement gate has
                // completed. Joining here therefore preserves actor progress
                // while the application drain is pending.
                if let Some(drain) = application_drain.take() {
                    let _ = drain.await;
                }
                break;
            }
            ActorCommand::Http {
                mut connection,
                bytes,
                reply,
            } => {
                // The generic transport byte helper deliberately has no
                // application-child argument. The executable adapter owns
                // that proof, so it routes each parsed request through the
                // child-aware HTTP entry point instead.
                let requests = match connection.push(&bytes) {
                    Ok(requests) => requests,
                    Err(error) => {
                        let _ = reply.send(Err(HttpConnectionError::Parse(error)));
                        continue;
                    }
                };
                let mut children = HostApplicationChildren::with_workers(
                    state.application_work.clone(),
                    state.application_workers.clone(),
                );
                let mut responses = Vec::with_capacity(requests.len());
                let mut failed = None;
                for request in requests {
                    let now = system_milliseconds();
                    let request = request.request().clone();
                    let response = if state.deletion.expired_deleted_lease(&request, now) {
                        expired_delete_response()
                    } else {
                        state
                            .transport
                            .handle_with_children(
                                request,
                                now,
                                &mut state.authority,
                                &mut state.issuer,
                                &mut state.deletion,
                                &mut children,
                            )
                            .await
                    };
                    match response.encode_http(TransportLimits::default()) {
                        Ok(response) => responses.push(response),
                        Err(error) => {
                            failed = Some(HttpConnectionError::Encode(error));
                            break;
                        }
                    }
                }
                if let Some(error) = failed {
                    let _ = reply.send(Err(error));
                    continue;
                }
                let Ok(retirement) =
                    capture_retirement_gates(&registry, state.transport.take_retired_attachments())
                else {
                    let _ = reply.send(Err(HttpConnectionError::Protocol(
                        orna_live_v1::Error::Closed,
                    )));
                    return;
                };
                let _ = reply.send(Ok(ActorHttpResult {
                    connection,
                    responses,
                    retirement,
                }));
            }
            ActorCommand::Begin {
                request,
                attachment,
                worker_id,
                reply,
            } => {
                // Register before transport admission. Because this is one
                // actor turn, a subsequent DELETE or replacement observes a
                // worker retirement gate for every successful reservation.
                // The registry is only a worker-lifecycle index; transport is
                // authoritative for active, pending, and retiring attachment
                // state. A duplicate registry key must therefore fail closed
                // without replacing its incumbent worker association.
                if state.admitted_upgrades.contains_key(&worker_id)
                    || !registry.borrow_mut().attach(attachment, worker_id)
                {
                    let _ = reply.send(Err(temporary_unavailable_response()));
                    continue;
                }
                let begun_wall_start = system_milliseconds();
                let result =
                    state
                        .transport
                        .begin_websocket_upgrade(&request, attachment, begun_wall_start);
                if result.is_err() {
                    registry
                        .borrow_mut()
                        .unregister_candidate(attachment, worker_id);
                } else if let Ok(upgrade) = &result {
                    state.admitted_upgrades.insert(
                        worker_id,
                        AdmittedUpgrade {
                            upgrade: upgrade.clone(),
                            begun_wall_start,
                        },
                    );
                }
                let Ok(retirement) =
                    capture_retirement_gates(&registry, state.transport.take_retired_attachments())
                else {
                    return;
                };
                // A failed Begin can retire a prior pending candidate. This
                // actor-owned observer acknowledges only after its join.
                if retirement_gates.unbounded_send(retirement).is_err() {
                    return;
                }
                let _ = reply.send(result);
            }
            ActorCommand::Deliver {
                upgrade,
                worker_id,
                reply,
            } => {
                let accepted_wall_start = system_milliseconds();
                let accepted = state
                    .admitted_upgrades
                    .get(&worker_id)
                    .is_some_and(|admitted| {
                        admitted.upgrade == upgrade
                            && accepted_wall_start >= admitted.begun_wall_start
                    })
                    && !state.delivering_upgrades.contains_key(&worker_id)
                    && state
                        .transport
                        .begin_websocket_upgrade_delivery(&upgrade, accepted_wall_start);
                if accepted {
                    state.delivering_upgrades.insert(
                        worker_id,
                        DeliveryStart {
                            upgrade,
                            accepted_wall_start,
                            accepted_instant: Instant::now(),
                        },
                    );
                }
                let _ = reply.send(accepted.then_some(()).ok_or(()));
            }
            ActorCommand::Commit {
                upgrade,
                worker_id,
                delivered_at_instant,
                reply,
            } => {
                if !state
                    .admitted_upgrades
                    .get(&worker_id)
                    .is_some_and(|admitted| admitted.upgrade == upgrade)
                    || state
                        .delivering_upgrades
                        .get(&worker_id)
                        .is_none_or(|delivery| delivery.upgrade != upgrade)
                {
                    let _ = reply.send(Err(()));
                    continue;
                }
                let delivery = state
                    .delivering_upgrades
                    .get(&worker_id)
                    .expect("delivery identity was checked above");
                // The worker captured the monotonic timestamp immediately
                // after the successful write and flush. The actor samples
                // the current wall and monotonic times here, so a stale
                // worker wall timestamp cannot bypass either deadline.
                // Cancellation observed after that delivery is intentionally
                // ignored at this commit point; pre-flush cancellation still
                // returns through Abort.
                let begun_wall_start = state
                    .admitted_upgrades
                    .get(&worker_id)
                    .expect("admission identity was checked above")
                    .begun_wall_start;
                let monotonic_window = delivery_window(
                    begun_wall_start,
                    delivery.accepted_wall_start,
                    delivery.upgrade.deadline(),
                );
                let committed_at = system_milliseconds();
                let committed_at_instant = Instant::now();
                let delivered_within_window = monotonic_window.is_some_and(|window| {
                    commit_within_delivery_window(
                        committed_at,
                        upgrade.deadline(),
                        delivery.accepted_instant,
                        delivered_at_instant,
                        committed_at_instant,
                        window,
                    )
                });
                let result = if delivered_within_window {
                    state
                        .transport
                        .commit_websocket_upgrade(upgrade.clone(), committed_at)
                        .await
                } else {
                    state.transport.abort_websocket_upgrade(&upgrade);
                    Err(orna_live_v1::Error::Closed)
                };
                state.admitted_upgrades.remove(&worker_id);
                state.delivering_upgrades.remove(&worker_id);
                let _ = state.transport.finish_websocket_upgrade_delivery(&upgrade);
                let Ok(retirement) =
                    capture_retirement_gates(&registry, state.transport.take_retired_attachments())
                else {
                    let _ = reply.send(Err(()));
                    return;
                };
                let Ok(response) = result else {
                    // A failed transport commit has already retired the
                    // candidate. Hand its supervisor-owned join gate to the
                    // outer retirement owner before reporting failure; if we
                    // drop it here, the attachment remains fenced forever
                    // after the worker exits.
                    if retirement_gates.unbounded_send(retirement).is_err() {
                        let _ = reply.send(Err(()));
                        return;
                    }
                    let _ = reply.send(Err(()));
                    continue;
                };
                let _ = reply.send(Ok((response, retirement)));
            }
            ActorCommand::Abort {
                upgrade,
                worker_id,
                reply,
            } => {
                if !state
                    .admitted_upgrades
                    .get(&worker_id)
                    .is_some_and(|admitted| admitted.upgrade == upgrade)
                {
                    let _ = reply.send(());
                    continue;
                }
                state.admitted_upgrades.remove(&worker_id);
                if state
                    .delivering_upgrades
                    .get(&worker_id)
                    .is_some_and(|delivery| delivery.upgrade == upgrade)
                {
                    let delivery = state
                        .delivering_upgrades
                        .remove(&worker_id)
                        .expect("delivery identity was checked above");
                    let _ = state
                        .transport
                        .finish_websocket_upgrade_delivery(&delivery.upgrade);
                }
                state.transport.abort_websocket_upgrade(&upgrade);
                let Ok(retirement) =
                    capture_retirement_gates(&registry, state.transport.take_retired_attachments())
                else {
                    return;
                };
                if retirement_gates.unbounded_send(retirement).is_err() {
                    return;
                }
                let _ = reply.send(());
            }
            ActorCommand::WorkerExited { worker_id, reply } => {
                let admitted = state.admitted_upgrades.remove(&worker_id);
                let delivery = state.delivering_upgrades.remove(&worker_id);
                if let Some(delivery) = &delivery {
                    let _ = state
                        .transport
                        .finish_websocket_upgrade_delivery(&delivery.upgrade);
                }
                if let Some(upgrade) = admitted
                    .map(|admitted| admitted.upgrade)
                    .or_else(|| delivery.map(|d| d.upgrade))
                {
                    state.transport.abort_websocket_upgrade(&upgrade);
                    let Ok(retirement) = capture_retirement_gates(
                        &registry,
                        state.transport.take_retired_attachments(),
                    ) else {
                        return;
                    };
                    if retirement_gates.unbounded_send(retirement).is_err() {
                        return;
                    }
                }
                // The worker waits for this reply before it returns to the
                // JoinSet, so an aborted candidate is captured while its
                // supervisor-owned termination gate is still available.
                let _ = reply.send(());
            }
            ActorCommand::Receive {
                mut socket,
                bytes,
                now,
                reply,
            } => {
                let preparation = state
                    .transport
                    .prepare_websocket_application(&mut socket, now, &bytes)
                    .await;
                match preparation {
                    Ok(WebSocketApplicationPreparation::Pending) => {
                        let _ = reply.send(Ok((socket, Vec::new())));
                    }
                    Ok(WebSocketApplicationPreparation::Output(output)) => {
                        let _ = reply.send(Ok((socket, vec![output])));
                    }
                    Ok(WebSocketApplicationPreparation::Work(ticket)) => {
                        schedule_application(
                            &mut state,
                            PendingApplication {
                                socket,
                                ticket,
                                reply,
                            },
                            &actor_sender,
                        );
                    }
                    Err(orna_live_v1::Error::InvalidMessage) => {
                        let _ = reply.send(Ok((
                            socket,
                            vec![WebSocketOutput::Close { code: Some(1002) }],
                        )));
                    }
                    Err(error) => {
                        let _ = reply.send(Err(error));
                    }
                }
            }
            ActorCommand::ApplicationComplete {
                socket,
                completion,
                reply,
            } => {
                let result = state.transport.complete_application(completion).await;
                let result = match result {
                    Ok(output) => Ok((socket, vec![output])),
                    // A canonical server-direction envelope, malformed CBOR,
                    // or a stale fenced completion is a protocol failure, not
                    // an application diagnostic. Send RFC 6455 1002 before
                    // this worker retires the attachment.
                    Err(orna_live_v1::Error::InvalidMessage) => {
                        Ok((socket, vec![WebSocketOutput::Close { code: Some(1002) }]))
                    }
                    Err(error) => Err(error),
                };
                let _ = reply.send(result);
            }
            ActorCommand::Close {
                attachment,
                now,
                reply,
            } => {
                let outcome = state.transport.close_attachment(attachment, now).await;
                let Ok(retirement) =
                    capture_retirement_gates(&registry, state.transport.take_retired_attachments())
                else {
                    return;
                };
                if retirement_gates.unbounded_send(retirement).is_err() {
                    return;
                }
                let _ = reply.send(outcome);
            }
        }
    }

    // The actor is the owner of all per-session evaluator workers. Once its
    // command channel closes, cancel every admitted session and join every
    // worker before returning.
    let _ = state.application_work.cancel_and_join_all().await;
    let _ = state.application_workers.stop_all().await;
}

/// Ends the host owner after admission has stopped. Every active socket worker
/// receives cancellation and is joined before the command channel closes and
/// the actor is joined. A caller-requested cancellation remains observable even
/// if a child also failed while shutdown was beginning.
async fn shutdown_concurrent_host(
    registry: &Rc<RefCell<WorkerRegistry>>,
    workers: &mut tokio::task::JoinSet<u64>,
    retirement_tasks: &mut tokio::task::JoinSet<()>,
    retirement_gates: &mut futures::channel::mpsc::UnboundedReceiver<RetirementGates>,
    retirement_acknowledgements: &futures::channel::mpsc::UnboundedSender<
        RetirementAcknowledgement,
    >,
    actor_sender: futures::channel::mpsc::UnboundedSender<ActorCommand>,
    actor: Option<tokio::task::JoinHandle<()>>,
    requested: LiveHostError,
) -> Result<(), LiveHostError> {
    registry.borrow_mut().cancel_all();
    // The actor must keep serving WorkerExited until every cancelled socket
    // worker has observed its acknowledgement. Drain application work first,
    // but reserve terminal shutdown for after those workers and their
    // retirement gates have been supervised.
    let _ = actor_sender.unbounded_send(ActorCommand::DrainApplicationWork);
    let mut worker_failed = false;
    while let Some(result) = workers.join_next_with_id().await {
        worker_failed |= acknowledge_worker_join(registry, result);
    }
    // Socket workers wait for their Close/Abort acknowledgement before they
    // join, so all meaningful actor-originated gates have been published by
    // this point. Drain any that were queued when shutdown preempted the
    // outer event loop before closing the actor's acknowledgement receiver.
    while let Some(Some(retirement)) = retirement_gates.next().now_or_never() {
        retirement_tasks.spawn_local(supervise_retirement_gates(
            retirement_acknowledgements.clone(),
            retirement,
        ));
    }
    // A successful worker join may unblock an actor-owned retirement gate.
    // Keep the actor and acknowledgement channel alive until every tracked
    // gate has either acknowledged the fence or observed a failed join.
    while retirement_tasks.join_next().await.is_some() {}
    let _ = actor_sender.unbounded_send(ActorCommand::Shutdown);
    drop(actor_sender);
    let actor_failed = match actor {
        Some(actor) => actor.await.is_err(),
        None => false,
    };
    if requested == LiveHostError::Cancelled {
        Err(LiveHostError::Cancelled)
    } else if worker_failed {
        Err(LiveHostError::Connection)
    } else if actor_failed {
        Err(LiveHostError::Runtime)
    } else {
        Err(requested)
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;
    use futures::io::AsyncWrite;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    struct FlushStall {
        written: Vec<u8>,
        flush_entered: Arc<AtomicBool>,
    }

    impl AsyncWrite for FlushStall {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.written.extend_from_slice(bytes);
            Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
            self.flush_entered.store(true, Ordering::Release);
            context.waker().wake_by_ref();
            Poll::Pending
        }

        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    struct FlushCancellation(Arc<AtomicBool>);

    impl Future for FlushCancellation {
        type Output = ();

        fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
            self.0
                .load(Ordering::Acquire)
                .then_some(())
                .map_or(Poll::Pending, Poll::Ready)
        }
    }

    pub(super) fn run_local(future: impl Future<Output = ()> + 'static) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime, future);
    }

    async fn acknowledgement_is_pending(
        acknowledgement: &mut futures::channel::oneshot::Receiver<Result<(), ()>>,
    ) -> bool {
        futures::future::poll_fn(|context| {
            Poll::Ready(Pin::new(&mut *acknowledgement).poll(context).is_pending())
        })
        .await
    }

    #[test]
    fn shutdown_cancels_and_joins_a_pending_worker() {
        run_local(async {
            let registry = Rc::new(RefCell::new(WorkerRegistry::default()));
            let (worker_id, cancellation) = registry.borrow_mut().register();
            let (finished_sender, finished) = futures::channel::oneshot::channel();
            let mut workers = tokio::task::JoinSet::new();
            let mut retirement_tasks = tokio::task::JoinSet::new();
            let task = workers.spawn_local(async move {
                cancellation.await.unwrap();
                let _ = finished_sender.send(());
                worker_id
            });
            registry.borrow_mut().bind_task(worker_id, task.id());
            let (actor_sender, actor_receiver) = futures::channel::mpsc::unbounded();
            let (retirement_acknowledgements, _retirement_acknowledgement_receiver) =
                futures::channel::mpsc::unbounded();
            let (_retirement_gate_sender, mut retirement_gates) =
                futures::channel::mpsc::unbounded();
            let actor = tokio::task::spawn_local(async move {
                use futures::StreamExt;
                let mut actor_receiver = actor_receiver;
                while actor_receiver.next().await.is_some() {}
            });

            assert_eq!(
                shutdown_concurrent_host(
                    &registry,
                    &mut workers,
                    &mut retirement_tasks,
                    &mut retirement_gates,
                    &retirement_acknowledgements,
                    actor_sender,
                    Some(actor),
                    LiveHostError::Cancelled,
                )
                .await,
                Err(LiveHostError::Cancelled)
            );
            assert_eq!(finished.now_or_never(), Some(Ok(())));
            assert!(workers.is_empty());
            assert!(registry.borrow().workers.is_empty());
        });
    }

    #[test]
    fn shutdown_joins_workers_before_reporting_a_panicked_actor() {
        run_local(async {
            let registry = Rc::new(RefCell::new(WorkerRegistry::default()));
            let (worker_id, cancellation) = registry.borrow_mut().register();
            let (finished_sender, finished) = futures::channel::oneshot::channel();
            let mut workers = tokio::task::JoinSet::new();
            let mut retirement_tasks = tokio::task::JoinSet::new();
            let task = workers.spawn_local(async move {
                cancellation.await.unwrap();
                let _ = finished_sender.send(());
                worker_id
            });
            registry.borrow_mut().bind_task(worker_id, task.id());
            let (actor_sender, _actor_receiver) = futures::channel::mpsc::unbounded();
            let (retirement_acknowledgements, _retirement_acknowledgement_receiver) =
                futures::channel::mpsc::unbounded();
            let (_retirement_gate_sender, mut retirement_gates) =
                futures::channel::mpsc::unbounded();
            let actor = tokio::task::spawn_local(async {
                panic!("actor termination is observed by its owner");
            });

            assert_eq!(
                shutdown_concurrent_host(
                    &registry,
                    &mut workers,
                    &mut retirement_tasks,
                    &mut retirement_gates,
                    &retirement_acknowledgements,
                    actor_sender,
                    Some(actor),
                    LiveHostError::Connection,
                )
                .await,
                Err(LiveHostError::Runtime)
            );
            assert_eq!(finished.now_or_never(), Some(Ok(())));
            assert!(workers.is_empty());
        });
    }

    #[test]
    fn shutdown_preserves_cancellation_after_a_worker_failure() {
        run_local(async {
            let registry = Rc::new(RefCell::new(WorkerRegistry::default()));
            let (worker_id, _cancellation) = registry.borrow_mut().register();
            let mut workers = tokio::task::JoinSet::<u64>::new();
            let mut retirement_tasks = tokio::task::JoinSet::new();
            let task = workers.spawn_local(async move {
                panic!("worker termination is observed by its owner");
            });
            registry.borrow_mut().bind_task(worker_id, task.id());
            let (actor_sender, actor_receiver) = futures::channel::mpsc::unbounded();
            let (retirement_acknowledgements, _retirement_acknowledgement_receiver) =
                futures::channel::mpsc::unbounded();
            let (_retirement_gate_sender, mut retirement_gates) =
                futures::channel::mpsc::unbounded();
            let actor = tokio::task::spawn_local(async move {
                use futures::StreamExt;
                let mut actor_receiver = actor_receiver;
                while actor_receiver.next().await.is_some() {}
            });

            assert_eq!(
                shutdown_concurrent_host(
                    &registry,
                    &mut workers,
                    &mut retirement_tasks,
                    &mut retirement_gates,
                    &retirement_acknowledgements,
                    actor_sender,
                    Some(actor),
                    LiveHostError::Cancelled,
                )
                .await,
                Err(LiveHostError::Cancelled)
            );
            assert!(workers.is_empty());
        });
    }

    #[test]
    fn shutdown_drains_queued_retirement_before_closing_acknowledgements() {
        run_local(async {
            let registry = Rc::new(RefCell::new(WorkerRegistry::default()));
            let mut workers = tokio::task::JoinSet::new();
            let mut retirement_tasks = tokio::task::JoinSet::new();
            let (completion, gate) = futures::channel::oneshot::channel();
            let (acknowledgements, mut acknowledgement_receiver) =
                futures::channel::mpsc::unbounded();
            let (observed_sender, observed) = futures::channel::oneshot::channel();
            let (actor_sender, _actor_receiver) = futures::channel::mpsc::unbounded();
            let actor = tokio::task::spawn_local(async move {
                use futures::StreamExt;
                match acknowledgement_receiver.next().await {
                    Some(RetirementAcknowledgement::Acknowledge { attachment, reply }) => {
                        assert_eq!(attachment, [17; 16]);
                        assert!(reply.send(true).is_ok());
                        assert!(observed_sender.send(()).is_ok());
                    }
                    _ => panic!("shutdown must retain the actor acknowledgement path"),
                }
            });
            let (retirement_gate_sender, mut retirement_gates) =
                futures::channel::mpsc::unbounded();
            retirement_gate_sender
                .unbounded_send(vec![RetirementGate {
                    attachment: [17; 16],
                    completion: gate,
                }])
                .unwrap();
            completion.send(Ok(())).unwrap();

            assert_eq!(
                shutdown_concurrent_host(
                    &registry,
                    &mut workers,
                    &mut retirement_tasks,
                    &mut retirement_gates,
                    &acknowledgements,
                    actor_sender,
                    Some(actor),
                    LiveHostError::Cancelled,
                )
                .await,
                Err(LiveHostError::Cancelled)
            );
            assert_eq!(observed.await, Ok(()));
        });
    }

    #[test]
    fn retirement_acknowledgement_waits_for_the_supervisor_join() {
        run_local(async {
            let registry = Rc::new(RefCell::new(WorkerRegistry::default()));
            let (worker_id, cancellation) = registry.borrow_mut().register();
            let attachment = [3; 16];
            registry.borrow_mut().attach(attachment, worker_id);
            let mut workers = tokio::task::JoinSet::new();
            let task = workers.spawn_local(async move {
                cancellation.await.unwrap();
                worker_id
            });
            registry.borrow_mut().bind_task(worker_id, task.id());

            let mut acknowledgement = registry
                .borrow_mut()
                .take_retired(vec![attachment])
                .unwrap()
                .pop()
                .unwrap()
                .completion;
            assert!(acknowledgement_is_pending(&mut acknowledgement).await);

            let joined = workers.join_next_with_id().await.unwrap();
            assert!(acknowledgement_is_pending(&mut acknowledgement).await);
            assert!(!acknowledge_worker_join(&registry, joined));
            assert_eq!(acknowledgement.now_or_never(), Some(Ok(Ok(()))));
        });
    }

    #[test]
    fn pending_handshake_candidate_has_the_same_retirement_gate() {
        run_local(async {
            let registry = Rc::new(RefCell::new(WorkerRegistry::default()));
            let (worker_id, cancellation) = registry.borrow_mut().register();
            let attachment = [13; 16];
            // Begin records the candidate before transport may publish it for
            // retirement, so DELETE can use the ordinary retirement queue.
            registry.borrow_mut().attach(attachment, worker_id);
            let mut workers = tokio::task::JoinSet::new();
            let task = workers.spawn_local(async move {
                cancellation.await.unwrap();
                worker_id
            });
            registry.borrow_mut().bind_task(worker_id, task.id());

            let retirement = capture_retirement_gates(&registry, vec![attachment]).unwrap();
            let joined = workers.join_next_with_id().await.unwrap();
            assert!(!acknowledge_worker_join(&registry, joined));
            let (acknowledgements, mut commands) = futures::channel::mpsc::unbounded();
            let acknowledgement = tokio::task::spawn_local(async move {
                use futures::StreamExt;
                match commands.next().await {
                    Some(RetirementAcknowledgement::Acknowledge { attachment, reply }) => {
                        assert_eq!(attachment, [13; 16]);
                        assert!(reply.send(true).is_ok());
                    }
                    _ => panic!("retirement join must be acknowledged by the actor"),
                }
            });
            assert_eq!(retire_and_join(&acknowledgements, retirement).await, Ok(()));
            acknowledgement.await.unwrap();
        });
    }

    #[test]
    fn failed_begin_unregisters_its_candidate() {
        let mut registry = WorkerRegistry::default();
        let (worker_id, _cancellation) = registry.register();
        let attachment = [14; 16];
        assert!(registry.attach(attachment, worker_id));
        registry.unregister_candidate(attachment, worker_id);
        assert!(
            capture_retirement_gates(&Rc::new(RefCell::new(registry)), vec![attachment]).is_err()
        );
    }

    #[test]
    fn rejected_candidate_cannot_unregister_an_incumbent_attachment() {
        let mut registry = WorkerRegistry::default();
        let (incumbent, _incumbent_cancellation) = registry.register();
        let (candidate, _candidate_cancellation) = registry.register();
        let attachment = [16; 16];

        assert!(registry.attach(attachment, incumbent));
        assert!(!registry.attach(attachment, candidate));
        registry.unregister_candidate(attachment, candidate);

        assert_eq!(registry.attachments.get(&attachment), Some(&incumbent));
    }

    #[test]
    fn deadline_cancellation_preserves_the_join_gate() {
        let mut registry = WorkerRegistry::default();
        let (worker_id, cancellation) = registry.register();
        let attachment = [17; 16];
        assert!(registry.attach(attachment, worker_id));
        registry.cancel(worker_id);
        assert_eq!(cancellation.now_or_never(), Some(Ok(())));
        assert!(registry.take_retired(vec![attachment]).is_ok());
    }

    #[test]
    fn cancellation_before_upgrade_write_keeps_the_handshake_undelivered() {
        run_local(async {
            let response = b"HTTP/1.1 101 Switching Protocols\r\n\r\n";
            let mut writer = FlushStall {
                written: Vec::new(),
                flush_entered: Arc::new(AtomicBool::new(false)),
            };
            let mut cancellation = futures::future::ready(());

            assert_eq!(
                deliver_websocket_upgrade_response(&mut writer, response, &mut cancellation).await,
                Err(())
            );
            // Cancellation is checked before the first write, so an
            // interrupted handshake has no externally visible prefix.
            assert!(writer.written.is_empty());
        });
    }

    #[test]
    fn cancellation_entered_during_upgrade_flush_keeps_handshake_uncommitted() {
        run_local(async {
            let response = b"HTTP/1.1 101 Switching Protocols\r\n\r\n";
            let flush_entered = Arc::new(AtomicBool::new(false));
            let mut writer = FlushStall {
                written: Vec::new(),
                flush_entered: Arc::clone(&flush_entered),
            };
            let mut cancellation = FlushCancellation(flush_entered);

            assert_eq!(
                deliver_websocket_upgrade_response(&mut writer, response, &mut cancellation).await,
                Err(())
            );
            assert_eq!(writer.written, response);
        });
    }

    #[test]
    fn panicked_worker_fails_its_retirement_acknowledgement() {
        run_local(async {
            let registry = Rc::new(RefCell::new(WorkerRegistry::default()));
            let (worker_id, cancellation) = registry.borrow_mut().register();
            let attachment = [4; 16];
            registry.borrow_mut().attach(attachment, worker_id);
            let mut workers = tokio::task::JoinSet::<u64>::new();
            let task = workers.spawn_local(async move {
                cancellation.await.unwrap();
                panic!("worker failure reaches the retirement owner");
            });
            registry.borrow_mut().bind_task(worker_id, task.id());

            let acknowledgement = registry
                .borrow_mut()
                .take_retired(vec![attachment])
                .unwrap()
                .pop()
                .unwrap()
                .completion;
            let joined = workers.join_next_with_id().await.unwrap();
            assert!(acknowledge_worker_join(&registry, joined));
            assert_eq!(acknowledgement.now_or_never(), Some(Ok(Err(()))));
        });
    }

    #[test]
    fn captured_retirement_survives_completion_before_requester_waits() {
        run_local(async {
            let registry = Rc::new(RefCell::new(WorkerRegistry::default()));
            let (worker_id, cancellation) = registry.borrow_mut().register();
            let attachment = [5; 16];
            registry.borrow_mut().attach(attachment, worker_id);
            let mut workers = tokio::task::JoinSet::new();
            let task = workers.spawn_local(async move {
                cancellation.await.unwrap();
                worker_id
            });
            registry.borrow_mut().bind_task(worker_id, task.id());

            let retirement = capture_retirement_gates(&registry, vec![attachment]).unwrap();
            let joined = workers.join_next_with_id().await.unwrap();
            assert!(!acknowledge_worker_join(&registry, joined));
            let (acknowledgements, mut commands) = futures::channel::mpsc::unbounded();
            let acknowledgement = tokio::task::spawn_local(async move {
                use futures::StreamExt;
                match commands.next().await {
                    Some(RetirementAcknowledgement::Acknowledge { attachment, reply }) => {
                        assert_eq!(attachment, [5; 16]);
                        assert!(reply.send(true).is_ok());
                    }
                    _ => panic!("retirement join must be acknowledged by the actor"),
                }
            });
            assert_eq!(retire_and_join(&acknowledgements, retirement).await, Ok(()));
            acknowledgement.await.unwrap();
        });
    }

    #[test]
    fn supervisor_owned_abort_expiry_and_replacement_gate_acknowledges_only_after_join() {
        run_local(async {
            let (acknowledgements, mut commands) = futures::channel::mpsc::unbounded();
            let (completion, gate) = futures::channel::oneshot::channel();
            let supervisor = tokio::task::spawn_local(supervise_retirement_gates(
                acknowledgements,
                vec![RetirementGate {
                    // Abort, expiry, and replacement all use this same
                    // unobserved gate path in the actor.
                    attachment: [8; 16],
                    completion: gate,
                }],
            ));
            use futures::StreamExt;
            assert!(commands.next().now_or_never().is_none());

            completion.send(Ok(())).unwrap();
            match commands.next().await {
                Some(RetirementAcknowledgement::Acknowledge { attachment, reply }) => {
                    assert_eq!(attachment, [8; 16]);
                    assert!(reply.send(true).is_ok());
                }
                _ => panic!("successful supervisor join must reach the actor"),
            }
            supervisor.await.unwrap();
        });
    }

    #[test]
    fn failed_supervisor_join_never_acknowledges_a_retired_attachment() {
        run_local(async {
            let (acknowledgements, mut commands) = futures::channel::mpsc::unbounded();
            let (completion, gate) = futures::channel::oneshot::channel();
            let supervisor = tokio::task::spawn_local(supervise_retirement_gates(
                acknowledgements,
                vec![RetirementGate {
                    attachment: [9; 16],
                    completion: gate,
                }],
            ));
            completion.send(Err(())).unwrap();
            supervisor.await.unwrap();
            use futures::StreamExt;
            // The supervisor owns the sole sender and closes the stream after
            // observing the failed join. Closure is evidence of no command;
            // `now_or_never()` therefore returns `Some(None)` here.
            assert!(matches!(commands.next().now_or_never(), Some(None)));
        });
    }

    #[test]
    fn dropped_retirement_gate_does_not_prevent_supervisor_cleanup() {
        run_local(async {
            let registry = Rc::new(RefCell::new(WorkerRegistry::default()));
            let (worker_id, cancellation) = registry.borrow_mut().register();
            let attachment = [6; 16];
            registry.borrow_mut().attach(attachment, worker_id);
            let mut workers = tokio::task::JoinSet::new();
            let task = workers.spawn_local(async move {
                cancellation.await.unwrap();
                worker_id
            });
            registry.borrow_mut().bind_task(worker_id, task.id());

            drop(capture_retirement_gates(&registry, vec![attachment]).unwrap());
            let joined = workers.join_next_with_id().await.unwrap();
            assert!(!acknowledge_worker_join(&registry, joined));
            let registry = registry.borrow();
            assert!(registry.workers.is_empty());
            assert!(registry.attachments.is_empty());
            assert!(registry.tasks.is_empty());
            assert!(registry.acknowledgements.is_empty());
        });
    }

    #[test]
    fn missing_retirement_mapping_is_an_invariant_failure() {
        let registry = Rc::new(RefCell::new(WorkerRegistry::default()));
        assert!(capture_retirement_gates(&registry, vec![[7; 16]]).is_err());
    }

    #[test]
    fn failed_retirement_join_suppresses_delete_success_response() {
        run_local(async {
            let (sender, acknowledgement) = futures::channel::oneshot::channel();
            sender.send(Err(())).unwrap();
            let mut writer = futures::io::Cursor::new(Vec::new());
            let mut cancellation = futures::future::pending();

            assert_eq!(
                write_http_after_retirement(
                    &futures::channel::mpsc::unbounded::<RetirementAcknowledgement>().0,
                    vec![RetirementGate {
                        attachment: [15; 16],
                        completion: acknowledgement,
                    }],
                    vec![b"HTTP/1.1 204 No Content\r\n\r\n".to_vec()],
                    &mut writer,
                    &mut cancellation,
                )
                .await,
                Err(())
            );
            assert!(writer.get_ref().is_empty());
        });
    }

    #[test]
    fn delete_success_response_waits_for_retired_worker_join() {
        run_local(async {
            let (completion_sender, completion) = futures::channel::oneshot::channel();
            let (acknowledgements, mut commands) = futures::channel::mpsc::unbounded();
            let acknowledgement = tokio::task::spawn_local(async move {
                use futures::StreamExt;
                match commands.next().await {
                    Some(RetirementAcknowledgement::Acknowledge { attachment, reply }) => {
                        assert_eq!(attachment, [16; 16]);
                        assert!(reply.send(true).is_ok());
                    }
                    _ => panic!("delete must wait for the worker supervisor"),
                }
            });
            let response = tokio::task::spawn_local(async move {
                let mut writer = futures::io::Cursor::new(Vec::new());
                let mut cancellation = futures::future::pending();
                let result = write_http_after_retirement(
                    &acknowledgements,
                    vec![RetirementGate {
                        attachment: [16; 16],
                        completion,
                    }],
                    vec![b"HTTP/1.1 204 No Content\r\n\r\n".to_vec()],
                    &mut writer,
                    &mut cancellation,
                )
                .await;
                (result, writer.into_inner())
            });

            tokio::task::yield_now().await;
            assert!(!response.is_finished());
            completion_sender.send(Ok(())).unwrap();
            assert_eq!(
                response.await.unwrap(),
                (Ok(()), b"HTTP/1.1 204 No Content\r\n\r\n".to_vec())
            );
            acknowledgement.await.unwrap();
        });
    }
}

struct WorkerSlot {
    cancellation: Option<futures::channel::oneshot::Sender<()>>,
    termination: futures::channel::oneshot::Receiver<Result<(), ()>>,
}

#[derive(Default)]
struct WorkerRegistry {
    next_id: u64,
    workers: BTreeMap<u64, WorkerSlot>,
    attachments: BTreeMap<[u8; 16], u64>,
    tasks: BTreeMap<tokio::task::Id, u64>,
    acknowledgements: BTreeMap<u64, futures::channel::oneshot::Sender<Result<(), ()>>>,
}

impl WorkerRegistry {
    fn register(&mut self) -> (u64, futures::channel::oneshot::Receiver<()>) {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let (cancellation, cancellation_receiver) = futures::channel::oneshot::channel();
        let (acknowledgement, termination) = futures::channel::oneshot::channel();
        self.workers.insert(
            id,
            WorkerSlot {
                cancellation: Some(cancellation),
                termination,
            },
        );
        self.acknowledgements.insert(id, acknowledgement);
        (id, cancellation_receiver)
    }

    fn bind_task(&mut self, id: u64, task: tokio::task::Id) {
        debug_assert!(self.workers.contains_key(&id));
        self.tasks.insert(task, id);
    }

    fn acknowledge(
        &mut self,
        task: tokio::task::Id,
        reported_id: Option<u64>,
        outcome: Result<(), ()>,
    ) -> bool {
        let Some(id) = self.tasks.remove(&task) else {
            return true;
        };
        let failed = outcome.is_err() || reported_id.is_some_and(|reported| reported != id);
        self.workers.remove(&id);
        self.attachments.retain(|_, worker| *worker != id);
        let Some(acknowledgement) = self.acknowledgements.remove(&id) else {
            return true;
        };
        let _ = acknowledgement.send(if failed { Err(()) } else { outcome });
        failed
    }

    /// Records a candidate only when it cannot replace another worker's
    /// attachment ownership. The transport remains the source of truth for
    /// protocol admission; this map prevents a rejected candidate from
    /// orphaning an already-owned worker.
    fn attach(&mut self, attachment: [u8; 16], id: u64) -> bool {
        if !self.workers.contains_key(&id) || self.attachments.contains_key(&attachment) {
            return false;
        }
        self.attachments.insert(attachment, id);
        true
    }

    fn detach(&mut self, attachment: [u8; 16], id: u64) {
        // The transport has closed this attachment, but the supervisor must
        // retain its worker association until the task is actually joined.
        // That makes a racing retirement wait for the same terminal join.
        debug_assert!(
            self.attachments
                .get(&attachment)
                .is_none_or(|worker| *worker == id)
        );
    }

    /// Removes a candidate that failed before it became transport-owned.
    /// Unlike [`Self::detach`], this is only used in the Begin actor turn;
    /// no retirement can have observed it yet.
    fn unregister_candidate(&mut self, attachment: [u8; 16], id: u64) {
        if self
            .attachments
            .get(&attachment)
            .is_some_and(|worker| *worker == id)
        {
            self.attachments.remove(&attachment);
        }
    }

    fn take_retired(&mut self, attachments: Vec<[u8; 16]>) -> Result<RetirementGates, ()> {
        let mut terminations = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            let Some(id) = self.attachments.get(&attachment).copied() else {
                return Err(());
            };
            let Some(slot) = self.workers.remove(&id) else {
                return Err(());
            };
            if let Some(cancellation) = slot.cancellation {
                let _ = cancellation.send(());
            }
            terminations.push(RetirementGate {
                attachment,
                completion: slot.termination,
            });
        }
        Ok(terminations)
    }

    fn cancel_all(&mut self) {
        for slot in self.workers.values_mut() {
            if let Some(cancellation) = slot.cancellation.take() {
                let _ = cancellation.send(());
            }
        }
    }

    /// Deadline cancellation revokes a worker's ability to extend a protected
    /// delivery window. Its slot remains registered so the actor can still
    /// capture the real supervisor join during the subsequent abort.
    fn cancel(&mut self, id: u64) {
        if let Some(slot) = self.workers.get_mut(&id)
            && let Some(cancellation) = slot.cancellation.take()
        {
            let _ = cancellation.send(());
        }
    }
}

/// Records a worker's actual JoinSet result. This is deliberately the sole
/// acknowledgement point: an owner observing a retirement waits for this
/// supervisor-side join, never for code running within the child itself.
fn acknowledge_worker_join(
    registry: &Rc<RefCell<WorkerRegistry>>,
    result: Result<(tokio::task::Id, u64), tokio::task::JoinError>,
) -> bool {
    match result {
        Ok((task, id)) => registry.borrow_mut().acknowledge(task, Some(id), Ok(())),
        Err(error) => registry.borrow_mut().acknowledge(error.id(), None, Err(())),
    }
}

/// Captures supervisor-owned retirement gates in the same actor turn that
/// removes attachments from transport state. The attachment identity remains
/// paired with its completion receiver so only the actor can release its
/// transport fence after the real worker supervisor joins it.
fn capture_retirement_gates(
    registry: &Rc<RefCell<WorkerRegistry>>,
    attachments: Vec<[u8; 16]>,
) -> Result<RetirementGates, ()> {
    registry.borrow_mut().take_retired(attachments)
}

async fn retire_and_join(
    acknowledgements: &futures::channel::mpsc::UnboundedSender<RetirementAcknowledgement>,
    retirement: RetirementGates,
) -> Result<(), ()> {
    let mut failed = false;
    for RetirementGate {
        attachment,
        completion,
    } in retirement
    {
        match completion.await {
            Ok(Ok(())) => {
                if acknowledge_retired_attachment(acknowledgements, attachment)
                    .await
                    .is_err()
                {
                    failed = true;
                }
            }
            Ok(Err(())) | Err(_) => failed = true,
        }
    }
    if failed { Err(()) } else { Ok(()) }
}

/// Owns retirement that has no requesting connection to wait for it (for
/// example a pending-upgrade abort or expiry). The outer host supervisor owns
/// and joins this future. Completion is still produced only by the outer
/// WorkerRegistry/JoinSet supervisor. A failed or dropped join deliberately
/// leaves the transport identity fenced.
async fn supervise_retirement_gates(
    acknowledgements: futures::channel::mpsc::UnboundedSender<RetirementAcknowledgement>,
    retirement: RetirementGates,
) {
    let _ = futures::future::join_all(retirement.into_iter().map(
        |RetirementGate {
             attachment,
             completion,
         }| {
            let acknowledgements = acknowledgements.clone();
            async move {
                if matches!(completion.await, Ok(Ok(()))) {
                    let _ = acknowledge_retired_attachment(&acknowledgements, attachment).await;
                }
            }
        },
    ))
    .await;
}

/// The live protocol's declared temporary-unavailable response. This is kept
/// byte-for-byte aligned with the transport's existing public wire shape so a
/// server-local worker-index collision does not introduce a new JSON protocol.
fn temporary_unavailable_response() -> orna_live_v1::WireResponse {
    orna_live_v1::WireResponse {
        status: 503,
        headers: vec![("content-type".into(), "application/json".into())],
        body: br#"{"code":"live.unavailable","message":"request rejected"}"#.to_vec(),
    }
}

/// Serializes retirement-fence release through the host actor. The sender is
/// called only after a gate received a successful supervisor-side join.
async fn acknowledge_retired_attachment(
    acknowledgements: &futures::channel::mpsc::UnboundedSender<RetirementAcknowledgement>,
    attachment: [u8; 16],
) -> Result<(), ()> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    acknowledgements
        .unbounded_send(RetirementAcknowledgement::Acknowledge {
            attachment,
            reply: sender,
        })
        .map_err(|_| ())?;
    receiver.await.map_err(|_| ())?.then_some(()).ok_or(())
}

async fn await_actor_response<T, C>(
    receiver: futures::channel::oneshot::Receiver<T>,
    cancellation: &mut C,
) -> Result<T, ()>
where
    C: Future<Output = ()> + Unpin,
{
    let mut receiver = receiver;
    futures::future::poll_fn(|context| {
        if Pin::new(&mut *cancellation).poll(context).is_ready() {
            return Poll::Ready(Err(()));
        }
        match Pin::new(&mut receiver).poll(context) {
            Poll::Ready(Ok(value)) => Poll::Ready(Ok(value)),
            Poll::Ready(Err(_)) => Poll::Ready(Err(())),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}

async fn serve_socket_worker(
    stream: tokio::net::TcpStream,
    actor: futures::channel::mpsc::UnboundedSender<ActorCommand>,
    retirement_acknowledgements: futures::channel::mpsc::UnboundedSender<RetirementAcknowledgement>,
    registry: Rc<RefCell<WorkerRegistry>>,
    worker_id: u64,
    cancellation_receiver: futures::channel::oneshot::Receiver<()>,
) {
    let (reader, writer) = stream.into_split();
    let reader = TokioReader(reader);
    let mut reader = PrefixedReader::new(reader);
    let writer = TokioWriter(writer);
    // A cancelled worker still closes its attachment through the actor. Fuse
    // the one-shot signal so cleanup can poll it without repolling a completed
    // receiver; after cancellation, the cleanup reply remains awaitable.
    let mut cancellation = cancellation_receiver.map(|_| ()).fuse();
    if let Ok(initial) = read_initial_request(&mut reader, &mut cancellation).await
        && let Some(parsed) = parse_http_request(&initial, TransportLimits::default())
            .ok()
            .flatten()
    {
        let request = parsed.request().clone();
        if request.method == "GET" && request.path.starts_with("/orna/live/") {
            serve_websocket_worker(
                reader,
                writer,
                initial,
                actor,
                retirement_acknowledgements,
                registry,
                worker_id,
                &mut cancellation,
            )
            .await;
        } else {
            serve_http_worker(
                reader,
                writer,
                initial,
                actor,
                retirement_acknowledgements,
                &mut cancellation,
            )
            .await;
        }
    }
}

async fn serve_http_worker<C>(
    mut reader: PrefixedReader<TokioReader>,
    mut writer: TokioWriter,
    initial: Vec<u8>,
    actor: futures::channel::mpsc::UnboundedSender<ActorCommand>,
    retirement_acknowledgements: futures::channel::mpsc::UnboundedSender<RetirementAcknowledgement>,
    cancellation: &mut C,
) where
    C: Future<Output = ()> + Unpin,
{
    let mut connection = HttpConnection::new(TransportLimits::default());
    if serve_http_bytes(
        &mut connection,
        &initial,
        &actor,
        &retirement_acknowledgements,
        &mut writer,
        cancellation,
    )
    .await
    .is_err()
    {
        return;
    }
    let mut chunk = [0; 8192];
    loop {
        let count = match await_socket_io(reader.read(&mut chunk), cancellation).await {
            Ok(count) => count,
            Err(()) => return,
        };
        if count == 0 {
            return;
        }
        if serve_http_bytes(
            &mut connection,
            &chunk[..count],
            &actor,
            &retirement_acknowledgements,
            &mut writer,
            cancellation,
        )
        .await
        .is_err()
        {
            return;
        }
    }
}

async fn serve_http_bytes<C>(
    connection: &mut HttpConnection,
    bytes: &[u8],
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    retirement_acknowledgements: &futures::channel::mpsc::UnboundedSender<
        RetirementAcknowledgement,
    >,
    writer: &mut TokioWriter,
    cancellation: &mut C,
) -> Result<(), ()>
where
    C: Future<Output = ()> + Unpin,
{
    let (returned, responses, retirement) =
        actor_http(actor, connection.clone(), bytes, cancellation).await?;
    *connection = returned;
    write_http_after_retirement(
        retirement_acknowledgements,
        retirement,
        responses,
        writer,
        cancellation,
    )
    .await
}

async fn write_http_after_retirement<W, C>(
    retirement_acknowledgements: &futures::channel::mpsc::UnboundedSender<
        RetirementAcknowledgement,
    >,
    retirement: RetirementGates,
    responses: Vec<Vec<u8>>,
    writer: &mut W,
    cancellation: &mut C,
) -> Result<(), ()>
where
    W: futures::io::AsyncWrite + Unpin,
    C: Future<Output = ()> + Unpin,
{
    retire_and_join(retirement_acknowledgements, retirement).await?;
    for response in responses {
        await_socket_io(writer.write_all(&response), cancellation).await?;
        await_socket_io(writer.flush(), cancellation).await?;
    }
    Ok(())
}

async fn actor_http(
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    connection: HttpConnection,
    bytes: &[u8],
    cancellation: &mut (impl Future<Output = ()> + Unpin),
) -> Result<(HttpConnection, Vec<Vec<u8>>, RetirementGates), ()> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    actor
        .unbounded_send(ActorCommand::Http {
            connection,
            bytes: bytes.to_vec(),
            reply: sender,
        })
        .map_err(|_| ())?;
    let result = await_actor_response(receiver, cancellation)
        .await?
        .map_err(|_| ())?;
    Ok((result.connection, result.responses, result.retirement))
}

async fn serve_websocket_worker<C>(
    mut reader: PrefixedReader<TokioReader>,
    mut writer: TokioWriter,
    initial: Vec<u8>,
    actor: futures::channel::mpsc::UnboundedSender<ActorCommand>,
    retirement_acknowledgements: futures::channel::mpsc::UnboundedSender<RetirementAcknowledgement>,
    registry: Rc<RefCell<WorkerRegistry>>,
    worker_id: u64,
    cancellation: &mut C,
) where
    C: Future<Output = ()> + Unpin,
{
    let Some(parsed) = parse_http_request(&initial, TransportLimits::default())
        .ok()
        .flatten()
    else {
        return;
    };
    let request = parsed.request().clone();
    let remainder = initial[parsed.consumed()..].to_vec();
    let attachment = match opaque_attachment() {
        Ok(attachment) => attachment,
        Err(()) => return,
    };
    let prepared = match actor_begin(&actor, request, attachment, worker_id).await {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(response)) => {
            let Ok(encoded) = response.encode_http(TransportLimits::default()) else {
                return;
            };
            let _ = await_socket_io(writer.write_all(&encoded), cancellation).await;
            let _ = await_socket_io(writer.flush(), cancellation).await;
            return;
        }
        Err(()) => return,
    };
    let response = prepared.response().clone();
    let encoded = match response.encode_http(TransportLimits::default()) {
        Ok(encoded) => encoded,
        Err(_) => {
            actor_abort(&actor, prepared, worker_id).await;
            return;
        }
    };
    // Enter the actor-owned barrier before a byte of 101 can be exposed.
    // From this point expiry and same-session lifecycle operations leave the
    // exact reservation to this worker until it commits or aborts.
    if actor_deliver(&actor, &prepared, worker_id).await.is_err() {
        actor_abort(&actor, prepared, worker_id).await;
        return;
    }
    #[cfg(test)]
    if !matches!(
        await_delivery_test_gate(worker_id, cancellation).await,
        Ok(DeliveryTestGateOutcome::Released)
    ) {
        actor_abort(&actor, prepared, worker_id).await;
        return;
    }
    if deliver_websocket_upgrade_response(&mut writer, &encoded, cancellation)
        .await
        .is_err()
    {
        actor_abort(&actor, prepared, worker_id).await;
        return;
    }
    let delivered_at_instant = Instant::now();
    if response.status != 101 {
        actor_abort(&actor, prepared, worker_id).await;
        return;
    }
    // Commit deliberately cannot be interrupted. Once the 101 response has
    // crossed the delivery boundary, abandoning its actor turn could leave an
    // attachment without a socket owner.
    let retirement = match actor_commit(&actor, prepared, worker_id, delivered_at_instant).await {
        Ok(retirement) => retirement,
        Err(()) => {
            registry.borrow_mut().detach(attachment, worker_id);
            return;
        }
    };
    if retire_and_join(&retirement_acknowledgements, retirement)
        .await
        .is_err()
    {
        close_worker_attachment(&actor, &registry, attachment, worker_id, cancellation).await;
        return;
    }
    let mut socket = WebSocketState::new(attachment);
    if !remainder.is_empty() {
        match serve_websocket_bytes(&actor, &mut socket, &remainder, &mut writer, cancellation)
            .await
        {
            Ok(false) => {}
            Ok(true) | Err(()) => {
                close_worker_attachment(&actor, &registry, attachment, worker_id, cancellation)
                    .await;
                return;
            }
        }
    }
    let mut chunk = [0; 8192];
    loop {
        let count = match await_socket_io(reader.read(&mut chunk), cancellation).await {
            Ok(count) => count,
            Err(()) => break,
        };
        if count == 0 {
            break;
        }
        match serve_websocket_bytes(
            &actor,
            &mut socket,
            &chunk[..count],
            &mut writer,
            cancellation,
        )
        .await
        {
            Ok(false) => {}
            Ok(true) | Err(()) => break,
        }
    }
    close_worker_attachment(&actor, &registry, attachment, worker_id, cancellation).await;
}

#[cfg(test)]
struct DeliveryTestGate {
    worker_id: u64,
    entered: futures::channel::oneshot::Sender<()>,
    release: futures::channel::oneshot::Receiver<()>,
}

#[cfg(test)]
enum DeliveryTestGateOutcome {
    Released,
    Cancelled,
}

#[cfg(test)]
thread_local! {
    static DELIVERY_TEST_GATE: RefCell<Option<DeliveryTestGate>> = const { RefCell::new(None) };
}

#[cfg(test)]
struct DeliveryTestGateGuard {
    worker_id: u64,
    release: Option<futures::channel::oneshot::Sender<()>>,
}

#[cfg(test)]
impl DeliveryTestGateGuard {
    fn install(worker_id: u64) -> (Self, futures::channel::oneshot::Receiver<()>) {
        let (entered, entered_receiver) = futures::channel::oneshot::channel();
        let (release, release_receiver) = futures::channel::oneshot::channel();
        DELIVERY_TEST_GATE.with(|slot| {
            let mut slot = slot.borrow_mut();
            assert!(
                slot.is_none(),
                "delivery test gate must not already be installed"
            );
            *slot = Some(DeliveryTestGate {
                worker_id,
                entered,
                release: release_receiver,
            });
        });
        (
            Self {
                worker_id,
                release: Some(release),
            },
            entered_receiver,
        )
    }

    fn release(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
    }
}

#[cfg(test)]
impl Drop for DeliveryTestGateGuard {
    fn drop(&mut self) {
        self.release();
        DELIVERY_TEST_GATE.with(|slot| {
            let mut slot = slot.borrow_mut();
            if slot
                .as_ref()
                .is_some_and(|gate| gate.worker_id == self.worker_id)
            {
                let _ = slot.take();
            }
        });
    }
}

#[cfg(test)]
async fn await_delivery_test_gate<C>(
    worker_id: u64,
    cancellation: &mut C,
) -> Result<DeliveryTestGateOutcome, ()>
where
    C: Future<Output = ()> + Unpin,
{
    let gate = DELIVERY_TEST_GATE.with(|slot| {
        let mut slot = slot.borrow_mut();
        slot.as_ref()
            .is_some_and(|gate| gate.worker_id == worker_id)
            .then(|| slot.take())
            .flatten()
    });
    if let Some(gate) = gate {
        let _ = gate.entered.send(());
        let mut release = gate.release;
        tokio::time::timeout(
            Duration::from_secs(2),
            futures::future::poll_fn(|context| {
                if Pin::new(&mut *cancellation).poll(context).is_ready() {
                    return Poll::Ready(Ok(DeliveryTestGateOutcome::Cancelled));
                }
                match Pin::new(&mut release).poll(context) {
                    Poll::Ready(Ok(())) => Poll::Ready(Ok(DeliveryTestGateOutcome::Released)),
                    Poll::Ready(Err(_)) => Poll::Ready(Err(())),
                    Poll::Pending => Poll::Pending,
                }
            }),
        )
        .await
        .map_err(|_| ())?
    } else {
        Ok(DeliveryTestGateOutcome::Released)
    }
}

async fn actor_begin(
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    request: orna_live_v1::WireRequest,
    attachment: [u8; 16],
    worker_id: u64,
) -> Result<Result<orna_live_v1::WebSocketUpgrade, orna_live_v1::WireResponse>, ()> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    actor
        .unbounded_send(ActorCommand::Begin {
            request,
            attachment,
            worker_id,
            reply: sender,
        })
        .map_err(|_| ())?;
    receiver.await.map_err(|_| ())
}

async fn actor_commit(
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    upgrade: orna_live_v1::WebSocketUpgrade,
    worker_id: u64,
    delivered_at_instant: Instant,
) -> Result<RetirementGates, ()> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    actor
        .unbounded_send(ActorCommand::Commit {
            upgrade,
            worker_id,
            delivered_at_instant,
            reply: sender,
        })
        .map_err(|_| ())?;
    let (_, retirement) = receiver.await.map_err(|_| ())?.map_err(|_| ())?;
    Ok(retirement)
}

/// Enters the actor-owned delivery-to-commit barrier before the worker writes
/// a provisional 101. This wait is deliberately non-cancellable: cancellation
/// before delivery still has to return through the matching Abort command.
async fn actor_deliver(
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    upgrade: &orna_live_v1::WebSocketUpgrade,
    worker_id: u64,
) -> Result<(), ()> {
    let (sender, receiver) = futures::channel::oneshot::channel();
    actor
        .unbounded_send(ActorCommand::Deliver {
            upgrade: upgrade.clone(),
            worker_id,
            reply: sender,
        })
        .map_err(|_| ())?;
    receiver.await.map_err(|_| ())?
}

/// Serializes abandonment of an opaque admission with the actor. This wait is
/// intentionally non-cancellable: callers invoke it only after Begin has
/// acknowledged ownership, and returning earlier would strand that admission.
async fn actor_abort(
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    upgrade: orna_live_v1::WebSocketUpgrade,
    worker_id: u64,
) {
    let (sender, receiver) = futures::channel::oneshot::channel();
    if actor
        .unbounded_send(ActorCommand::Abort {
            upgrade,
            worker_id,
            reply: sender,
        })
        .is_ok()
    {
        let _ = receiver.await;
    }
}

/// Makes worker exit observable to the actor before the outer supervisor can
/// join it. If the worker died while holding a delivery barrier, this drives
/// the reservation through abort and captures its retirement gate first.
async fn actor_worker_exited(
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    worker_id: u64,
) {
    let (sender, receiver) = futures::channel::oneshot::channel();
    if actor
        .unbounded_send(ActorCommand::WorkerExited {
            worker_id,
            reply: sender,
        })
        .is_ok()
    {
        let _ = receiver.await;
    }
}

/// Delivers the provisional HTTP response for a WebSocket handoff.
///
/// The caller owns the reservation and must abort it when this returns an
/// error. In particular, a successful write without a successful flush has
/// not crossed the transport delivery boundary and must not be committed.
async fn deliver_websocket_upgrade_response<W, C>(
    writer: &mut W,
    response: &[u8],
    cancellation: &mut C,
) -> Result<(), ()>
where
    W: futures::io::AsyncWrite + Unpin,
    C: Future<Output = ()> + Unpin,
{
    await_socket_io(writer.write_all(response), cancellation).await?;
    await_socket_io(writer.flush(), cancellation).await
}

async fn serve_websocket_bytes<C, W>(
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    socket: &mut WebSocketState,
    bytes: &[u8],
    writer: &mut W,
    cancellation: &mut C,
) -> Result<bool, ()>
where
    C: Future<Output = ()> + Unpin,
    W: futures::io::AsyncWrite + Unpin,
{
    let mut input = Some(bytes);
    loop {
        let (sender, receiver) = futures::channel::oneshot::channel();
        actor
            .unbounded_send(ActorCommand::Receive {
                socket: std::mem::replace(socket, WebSocketState::new([0; 16])),
                bytes: input.take().unwrap_or_default().to_vec(),
                now: system_milliseconds(),
                reply: sender,
            })
            .map_err(|_| ())?;
        let (returned, outputs) = await_actor_response(receiver, cancellation)
            .await?
            .map_err(|_| ())?;
        *socket = returned;
        for output in outputs {
            let closing = matches!(output, WebSocketOutput::Close { .. });
            let Some(frame) =
                encode_websocket_output(&output, TransportLimits::default()).map_err(|_| ())?
            else {
                continue;
            };
            await_socket_io(writer.write_all(&frame), cancellation).await?;
            await_socket_io(writer.flush(), cancellation).await?;
            if closing {
                await_socket_io(writer.close(), cancellation).await?;
                return Ok(true);
            }
        }
        if !socket
            .has_complete_frame(TransportLimits::default().max_frame_bytes)
            .map_err(|_| ())?
        {
            return Ok(false);
        }
    }
}

async fn close_worker_attachment(
    actor: &futures::channel::mpsc::UnboundedSender<ActorCommand>,
    registry: &Rc<RefCell<WorkerRegistry>>,
    attachment: [u8; 16],
    worker_id: u64,
    _cancellation: &mut (impl Future<Output = ()> + Unpin),
) {
    let (sender, receiver) = futures::channel::oneshot::channel();
    if actor
        .unbounded_send(ActorCommand::Close {
            attachment,
            now: system_milliseconds(),
            reply: sender,
        })
        .is_ok()
    {
        // A committed attachment must be closed in actor order even when this
        // worker is itself retiring. The acknowledgement is intentionally not
        // raced with cancellation: cancellation is the reason this cleanup is
        // required, not permission to abandon it.
        let _ = receiver.await;
    }
    registry.borrow_mut().detach(attachment, worker_id);
}

async fn await_socket_io<T, F, C>(operation: F, cancellation: &mut C) -> Result<T, ()>
where
    F: Future<Output = io::Result<T>> + Unpin,
    C: Future<Output = ()> + Unpin,
{
    let mut operation = operation;
    futures::future::poll_fn(|context| {
        if Pin::new(&mut *cancellation).poll(context).is_ready() {
            return Poll::Ready(Err(()));
        }
        Pin::new(&mut operation).poll(context).map_err(|_| ())
    })
    .await
}

async fn await_host_io<T, F, C>(operation: F, cancellation: &mut C) -> Result<T, LiveHostError>
where
    F: Future<Output = io::Result<T>> + Unpin,
    C: Future<Output = ()> + Unpin,
{
    let mut operation = operation;
    futures::future::poll_fn(|context| {
        if Pin::new(&mut *cancellation).poll(context).is_ready() {
            return Poll::Ready(Err(LiveHostError::Cancelled));
        }
        Pin::new(&mut operation)
            .poll(context)
            .map_err(|_| LiveHostError::Connection)
    })
    .await
}

fn map_connection_error(error: HttpIoError) -> LiveHostError {
    match error {
        HttpIoError::Cancelled => LiveHostError::Cancelled,
        _ => LiveHostError::Connection,
    }
}

async fn read_initial_request<C>(
    reader: &mut (impl futures::io::AsyncRead + Unpin),
    cancellation: &mut C,
) -> Result<Vec<u8>, LiveHostError>
where
    C: Future<Output = ()> + Unpin,
{
    let limits = TransportLimits::default();
    let mut bytes = Vec::new();
    let mut chunk = [0; 8192];
    loop {
        match parse_http_request(&bytes, limits) {
            Ok(Some(_)) => return Ok(bytes),
            Ok(None) => {}
            Err(_) => return Err(LiveHostError::Connection),
        }
        let mut read = reader.read(&mut chunk);
        let count = futures::future::poll_fn(|context| {
            if Pin::new(&mut *cancellation).poll(context).is_ready() {
                return Poll::Ready(Err(LiveHostError::Cancelled));
            }
            Pin::new(&mut read)
                .poll(context)
                .map(|result| result.map_err(|_| LiveHostError::Connection))
        })
        .await?;
        if count == 0 {
            return Err(LiveHostError::Connection);
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
}

fn opaque_attachment() -> Result<[u8; 16], ()> {
    let mut attachment = [0; 16];
    for _ in 0..8 {
        getrandom::fill(&mut attachment).map_err(|_| ())?;
        if attachment != [0; 16] {
            return Ok(attachment);
        }
    }
    Err(())
}

struct TokioReader(tokio::net::tcp::OwnedReadHalf);

impl futures::io::AsyncRead for TokioReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let mut read_buffer = tokio::io::ReadBuf::new(buffer);
        tokio::io::AsyncRead::poll_read(Pin::new(&mut self.0), context, &mut read_buffer)
            .map(|result| result.map(|()| read_buffer.filled().len()))
    }
}

struct TokioWriter(tokio::net::tcp::OwnedWriteHalf);

impl futures::io::AsyncWrite for TokioWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        tokio::io::AsyncWrite::poll_write(Pin::new(&mut self.0), context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_flush(Pin::new(&mut self.0), context)
    }

    fn poll_close(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_shutdown(Pin::new(&mut self.0), context)
    }
}

struct PrefixedReader<R> {
    prefix: Vec<u8>,
    offset: usize,
    reader: R,
}

impl<R> PrefixedReader<R> {
    fn new(reader: R) -> Self {
        Self {
            prefix: Vec::new(),
            offset: 0,
            reader,
        }
    }

    fn replay(&mut self, prefix: Vec<u8>) {
        self.prefix = prefix;
        self.offset = 0;
    }
}

impl<R> futures::io::AsyncRead for PrefixedReader<R>
where
    R: futures::io::AsyncRead + Unpin,
{
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        if self.offset < self.prefix.len() {
            let count = (self.prefix.len() - self.offset).min(buffer.len());
            buffer[..count].copy_from_slice(&self.prefix[self.offset..self.offset + count]);
            self.offset += count;
            return Poll::Ready(Ok(count));
        }
        Pin::new(&mut self.reader).poll_read(context, buffer)
    }
}

struct HostAuthority {
    database_id: [u8; 16],
    runtime_id: [u8; 16],
    expiries: SessionExpiries,
}

impl LiveSessionAuthority for HostAuthority {
    fn create_session(
        &mut self,
        database: [u8; 16],
        now: u64,
    ) -> orna_live_v1::Result<SessionMetadata> {
        if database != self.database_id {
            return Err(orna_live_v1::Error::Denied);
        }
        let mut id = [0; 16];
        for _ in 0..8 {
            getrandom::fill(&mut id).map_err(|_| orna_live_v1::Error::Denied)?;
            if id != [0; 16] {
                let session = SessionId::new(id);
                let expires_at = now.saturating_add(30_000);
                if !self.expiries.borrow().contains_key(&session) {
                    self.expiries.borrow_mut().insert(session, expires_at);
                    return Ok(SessionMetadata {
                        session: id,
                        database,
                        runtime: self.runtime_id,
                        expires_at,
                        subscribe: subscribe_payload(),
                    });
                }
            }
        }
        Err(orna_live_v1::Error::Denied)
    }
}

struct HostDeletion {
    expiries: SessionExpiries,
    deleted_leases: DeletedLeaseIndex,
    application: SharedApplication,
    application_workers: Option<ApplicationWorkerRegistry>,
}

impl HostDeletion {
    /// The transport keeps a short idempotency record after successful
    /// deletion. Its record intentionally contains only bearer/origin match
    /// data, so the executable adapter retains the original lease deadline
    /// and prevents that cache from extending authentication past expiry.
    fn expired_deleted_lease(&self, request: &orna_live_v1::WireRequest, now: u64) -> bool {
        if request.method != "DELETE" {
            return false;
        }
        let Some(id) = parse_session_delete_path(&request.path) else {
            return false;
        };
        self.deleted_leases
            .borrow()
            .get(&SessionId::new(id))
            .is_some_and(|expires_at| *expires_at <= now)
    }
}

/// Executable ownership boundary shared by application callbacks and DELETE.
///
/// The live protocol owns admission and durable request fencing; this shared
/// supervisor owns the callback/descendant leases. `HostDeletion` still takes
/// the evaluator's unique mutable owner before removing its retained state, so
/// a successful deletion has both application-work and evaluator-ownership
/// evidence.
struct HostApplicationChildren {
    supervisor: LiveApplicationWorkSupervisor,
    workers: Option<ApplicationWorkerRegistry>,
}

impl HostApplicationChildren {
    fn new(supervisor: LiveApplicationWorkSupervisor) -> Self {
        Self {
            supervisor,
            workers: None,
        }
    }

    fn with_workers(
        supervisor: LiveApplicationWorkSupervisor,
        workers: ApplicationWorkerRegistry,
    ) -> Self {
        Self {
            supervisor,
            workers: Some(workers),
        }
    }
}

impl LiveSessionChildren for HostApplicationChildren {
    fn cancel_and_join_session<'a>(
        &'a mut self,
        session: [u8; 16],
        requests: &'a [RequestIdentity],
    ) -> Pin<Box<dyn Future<Output = orna_live_v1::Result<()>> + 'a>> {
        if requests.iter().any(|request| request.session_id != session) {
            return Box::pin(async { Err(orna_live_v1::Error::DeletionFailed) });
        }
        let supervisor = self.supervisor.clone();
        let workers = self.workers.clone();
        Box::pin(async move {
            let drained = supervisor.cancel_and_join(session).await;
            if drained.is_err()
                && let Some(workers) = workers
            {
                let _ = workers.stop_and_join(SessionId::new(session)).await;
            }
            drained
        })
    }
}

impl SessionDeletionAdapter for HostDeletion {
    type Error = ();

    fn delete(&mut self, session: SessionId) -> Result<(), Self::Error> {
        // Obtain the application owner before mutating the retained lease
        // records. A failed ownership proof must leave the adapter retryable
        // and cannot create an idempotency record for incomplete cleanup.
        let mut application = self.application.try_borrow_mut().map_err(|_| ())?;
        let application = application.as_mut().ok_or(())?;
        let expires_at = self.expiries.borrow().get(&session).copied().ok_or(())?;
        if let Some(workers) = &self.application_workers {
            workers.stop_and_join_blocking(session).map_err(|_| ())?;
        }
        self.expiries.borrow_mut().remove(&session);
        self.deleted_leases.borrow_mut().insert(session, expires_at);
        application.remove(session);
        Ok(())
    }
}

fn expired_delete_response() -> orna_live_v1::WireResponse {
    orna_live_v1::WireResponse {
        status: 410,
        headers: Vec::new(),
        body: b"live.expired".to_vec(),
    }
}

fn parse_session_delete_path(path: &str) -> Option<[u8; 16]> {
    let value = path.strip_prefix("/orna/session/")?;
    if value.len() != 36
        || ![8, 13, 18, 23]
            .into_iter()
            .all(|index| value.as_bytes()[index] == b'-')
    {
        return None;
    }
    let mut id = [0; 16];
    let mut digits = value.bytes().filter(|byte| *byte != b'-');
    for byte in &mut id {
        *byte = (hex_digit(digits.next()?)? << 4) | hex_digit(digits.next()?)?;
    }
    digits.next().is_none().then_some(id)
}

const fn hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn runtime_identity(database_id: [u8; 16]) -> (RuntimeIdentity, [u8; 32]) {
    let mut repository_id = database_id;
    for (index, byte) in repository_id.iter_mut().enumerate() {
        let rotation = u32::try_from(index % 7 + 1).expect("bounded rotation");
        let salt = u8::try_from(index).expect("fixed identity length");
        *byte = byte.rotate_left(rotation) ^ (0x5a_u8.wrapping_add(salt));
    }
    if repository_id == [0; 16] {
        repository_id[0] = 1;
    }
    let mut initial_digest = [0; 32];
    initial_digest[..16].copy_from_slice(&database_id);
    initial_digest[16..].copy_from_slice(&repository_id);
    (
        RuntimeIdentity {
            database_id,
            repository_id,
        },
        initial_digest,
    )
}

fn subscribe_payload() -> Vec<u8> {
    Envelope {
        request: Some([1; 16]),
        watch: None,
        message: Message::Subscribe {
            resource: [2; 16],
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "terminal/dark".into(),
                supported_kinds: vec![],
            },
        },
        extensions: std::collections::BTreeMap::new(),
    }
    .encode(ProtocolLimits::default())
    .expect("static subscribe payload")
}

fn system_milliseconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, duration_milliseconds)
}

fn delivery_window(
    begun_wall_start: u64,
    accepted_wall_start: u64,
    deadline: u64,
) -> Option<Duration> {
    (accepted_wall_start >= begun_wall_start)
        .then(|| deadline.checked_sub(accepted_wall_start))
        .flatten()
        .map(Duration::from_millis)
}

fn commit_within_delivery_window(
    committed_at: u64,
    deadline: u64,
    accepted_instant: Instant,
    delivered_at_instant: Instant,
    committed_at_instant: Instant,
    window: Duration,
) -> bool {
    committed_at_instant
        .checked_duration_since(accepted_instant)
        .is_some_and(|elapsed| {
            committed_at < deadline
                && elapsed < window
                && delivered_at_instant
                    .checked_duration_since(accepted_instant)
                    .is_some_and(|elapsed| elapsed < window)
        })
}

fn duration_milliseconds(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        ActorCommand, ApplicationJob, ApplicationWorkerRecipe, ApplicationWorkerRegistry,
        DeletedLeaseIndex, HostApplicationChildren, HostDeletion, SharedApplication,
        commit_within_delivery_window, delivery_window, duration_milliseconds,
        expired_delete_response, runtime_identity, subscribe_payload,
    };
    use crate::live_eval::PureEvalApplication;
    use futures::StreamExt;
    use orna_live_v1::{
        ApplicationPreparation, CreateRequest, Frame, HttpConnection, Limits, LiveCredentialIssuer,
        LiveHost, LiveTransport, ResumeRequest, SessionCredential, SystemCredentialIssuer,
        TransportLimits, WebSocketState, WireRequest,
    };
    use orna_project_v1::ProjectLoader;
    use orna_protocol_v1::{
        DatabaseContext, Envelope, Limits as ProtocolLimits, Message, PresentationContext,
        canonical_request_fingerprint,
    };
    use orna_repository_v1::{initialize_repository, inspect_metadata};
    use orna_runtime_v1::RuntimeState;
    use orna_security_v1::{
        Origin, OriginPolicy, SessionBoundary, SessionDeletionAdapter, SessionId,
    };
    use orna_serving_v1::Serving;
    use std::time::{Duration, Instant};
    use std::{
        cell::RefCell,
        collections::BTreeMap,
        fs, io,
        net::TcpListener,
        path::PathBuf,
        rc::Rc,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
    };
    use tokio::io::{AsyncReadExt as TokioAsyncReadExt, AsyncWriteExt as TokioAsyncWriteExt};

    async fn bounded_test_wait<T>(future: impl Future<Output = T>, operation: &str) -> T {
        tokio::time::timeout(Duration::from_secs(2), future)
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {operation}"))
    }

    async fn read_http_headers(
        stream: &mut tokio::net::TcpStream,
        mut readiness: Option<futures::channel::oneshot::Sender<()>>,
    ) -> io::Result<Vec<u8>> {
        let mut response = Vec::new();
        let mut byte = [0; 1];
        while !response.ends_with(b"\r\n\r\n") {
            if let Some(readiness) = readiness.take() {
                let _ = readiness.send(());
            }
            bounded_test_wait(stream.read_exact(&mut byte), "HTTP response header byte").await?;
            response.extend_from_slice(&byte);
        }
        Ok(response)
    }

    // Admit through HTTP so both the host lease and transport session record
    // exist before the actor begins WebSocket delivery.
    async fn admit_test_session(
        transport: &mut LiveTransport,
        deletion: &mut HostDeletion,
        now: u64,
        metadata: orna_live_v1::SessionMetadata,
    ) -> [u8; 32] {
        struct Authority(orna_live_v1::SessionMetadata);

        impl orna_live_v1::LiveSessionAuthority for Authority {
            fn create_session(
                &mut self,
                _: [u8; 16],
                _: u64,
            ) -> orna_live_v1::Result<orna_live_v1::SessionMetadata> {
                Ok(self.0.clone())
            }
        }

        let mut database = String::with_capacity(36);
        for (index, byte) in metadata.database.into_iter().enumerate() {
            if [4, 6, 8, 10].contains(&index) {
                database.push('-');
            }
            database.push(b"0123456789abcdef"[(byte >> 4) as usize] as char);
            database.push(b"0123456789abcdef"[(byte & 15) as usize] as char);
        }
        let mut issuer = SystemCredentialIssuer::default();
        let created = transport
            .handle(
                WireRequest {
                    method: "POST".into(),
                    path: "/orna/session".into(),
                    headers: vec![
                        ("origin".into(), "https://app.example".into()),
                        ("content-type".into(), "application/json".into()),
                    ],
                    body: format!(r#"{{"database":"{database}","protocol":"orna.present.v1"}}"#)
                        .into_bytes(),
                },
                now,
                &mut Authority(metadata),
                &mut issuer,
                deletion,
            )
            .await;
        assert_eq!(created.status, 201);
        issuer.last_issued().unwrap()
    }

    #[test]
    fn delivery_test_gate_install_preserves_an_existing_gate_on_panic() {
        let (guard, _entered) = super::DeliveryTestGateGuard::install(61);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = super::DeliveryTestGateGuard::install(62);
        }));
        assert!(result.is_err());
        drop(guard);
    }

    #[test]
    fn live_clock_uses_milliseconds_for_the_advertised_lease() {
        assert_eq!(duration_milliseconds(Duration::from_secs(30)), 30_000);
    }

    #[test]
    fn delivery_window_rejects_wall_clock_rollback() {
        assert_eq!(
            delivery_window(1_000, 900, 2_000),
            None,
            "a Deliver timestamp earlier than Begin must fail closed"
        );
        assert_eq!(
            delivery_window(1_000, 1_100, 2_000),
            Some(Duration::from_millis(900))
        );
    }

    #[test]
    fn commit_window_rejects_actor_turn_after_admission_deadline() {
        let accepted = Instant::now();
        let delivered = accepted.checked_add(Duration::from_millis(99)).unwrap();
        let committed = accepted.checked_add(Duration::from_millis(101)).unwrap();

        assert!(!commit_within_delivery_window(
            1_099,
            1_100,
            accepted,
            delivered,
            committed,
            Duration::from_millis(100)
        ));
        assert!(commit_within_delivery_window(
            1_099,
            1_100,
            accepted,
            delivered,
            delivered,
            Duration::from_millis(100)
        ));
    }

    #[test]
    fn expired_deleted_lease_blocks_transport_idempotency_cache() {
        let id = [7; 16];
        let deleted_leases: DeletedLeaseIndex =
            Rc::new(RefCell::new(BTreeMap::from([(SessionId::new(id), 100)])));
        let deletion = HostDeletion {
            expiries: Rc::new(RefCell::new(BTreeMap::new())),
            deleted_leases,
            application: Rc::new(RefCell::new(None)) as SharedApplication,
            application_workers: None,
        };
        let request = orna_live_v1::WireRequest {
            method: "DELETE".into(),
            path: "/orna/session/07070707-0707-0707-0707-070707070707".into(),
            headers: vec![
                ("origin".into(), "http://localhost".into()),
                ("authorization".into(), "Bearer retained".into()),
            ],
            body: Vec::new(),
        };

        assert!(!deletion.expired_deleted_lease(&request, 99));
        assert!(deletion.expired_deleted_lease(&request, 100));
        assert_eq!(expired_delete_response().status, 410);
    }

    #[test]
    fn unavailable_application_does_not_partially_delete_session_lease() {
        let session = SessionId::new([8; 16]);
        let expiries = Rc::new(RefCell::new(BTreeMap::from([(session, 100)])));
        let deleted_leases: DeletedLeaseIndex = Rc::new(RefCell::new(BTreeMap::new()));
        let mut deletion = HostDeletion {
            expiries: Rc::clone(&expiries),
            deleted_leases: Rc::clone(&deleted_leases),
            application: Rc::new(RefCell::new(None)) as SharedApplication,
            application_workers: None,
        };

        assert_eq!(deletion.delete(session), Err(()));
        assert_eq!(expiries.borrow().get(&session), Some(&100));
        assert!(deleted_leases.borrow().is_empty());
    }

    #[test]
    fn post_flush_cancellation_commits_the_delivered_upgrade() {
        let repository = worker_repository();
        let session = [21; 16];
        let attachment = [22; 16];
        let now = super::system_milliseconds();
        let expires_at = now.saturating_add(60_000);
        let origin = Origin::parse("https://app.example").unwrap();
        let expiries = Rc::new(RefCell::new(BTreeMap::from([(
            SessionId::new(session),
            expires_at,
        )])));
        let host = LiveHost::new(
            Limits::default(),
            SessionBoundary::new(OriginPolicy::new([origin.clone()], []), 60_000),
            Serving::new(orna_serving_v1::Limits::default()).unwrap(),
        )
        .unwrap();
        let mut transport = LiveTransport::new(host, TransportLimits::default()).unwrap();
        let application = PureEvalApplication::from_repository_with_project(
            &repository.recipe.repository,
            repository.recipe.database_id,
            repository.recipe.identity,
            repository.recipe.initial_digest,
            repository.recipe.runtime_owner,
            Rc::clone(&expiries),
            Some(repository.recipe.project.clone()),
            Some(repository.recipe.capture.clone()),
        )
        .unwrap();
        let application_work = transport.application_work_supervisor();
        let application_workers = ApplicationWorkerRegistry::new(repository.recipe.clone());
        let mut deletion = HostDeletion {
            expiries: Rc::clone(&expiries),
            deleted_leases: Rc::new(RefCell::new(BTreeMap::new())),
            application: Rc::new(RefCell::new(Some(application))),
            application_workers: Some(application_workers.clone()),
        };
        let token = futures::executor::block_on(admit_test_session(
            &mut transport,
            &mut deletion,
            now,
            orna_live_v1::SessionMetadata {
                session,
                database: repository.recipe.database_id,
                runtime: [23; 16],
                expires_at,
                subscribe: subscribe_payload(),
            },
        ));
        let mut token_text = String::with_capacity(43);
        const BASE64URL: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        for chunk in token.chunks(3) {
            token_text.push(BASE64URL[(chunk[0] >> 2) as usize] as char);
            token_text.push(
                BASE64URL
                    [(((chunk[0] & 3) << 4) | (chunk.get(1).copied().unwrap_or(0) >> 4)) as usize]
                    as char,
            );
            if chunk.len() > 1 {
                token_text.push(
                    BASE64URL[(((chunk[1] & 15) << 2) | (chunk.get(2).copied().unwrap_or(0) >> 6))
                        as usize] as char,
                );
            }
            if chunk.len() > 2 {
                token_text.push(BASE64URL[(chunk[2] & 63) as usize] as char);
            }
        }
        let session_path = "/orna/live/15151515-1515-1515-1515-151515151515";
        let upgrade = WireRequest {
            method: "GET".into(),
            path: session_path.into(),
            headers: vec![
                ("origin".into(), "https://app.example".into()),
                ("connection".into(), "Upgrade".into()),
                ("upgrade".into(), "websocket".into()),
                ("sec-websocket-version".into(), "13".into()),
                (
                    "sec-websocket-key".into(),
                    "dGhlIHNhbXBsZSBub25jZQ==".into(),
                ),
                ("sec-websocket-protocol".into(), "orna.present.v1".into()),
                ("cookie".into(), format!("orna_session={token_text}")),
            ],
            body: Vec::new(),
        };
        let delete_request = format!(
            "DELETE /orna/session/15151515-1515-1515-1515-151515151515 HTTP/1.1\r\nHost: app.example\r\nOrigin: {}\r\nAuthorization: Bearer {token_text}\r\n\r\n",
            "https://app.example"
        );
        let resume_body =
            format!(r#"{{"resume_token":"{token_text}","protocol":"orna.present.v1"}}"#);
        let resume_request = format!(
            "POST /orna/session/15151515-1515-1515-1515-151515151515/resume HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{resume_body}",
            resume_body.len(),
        );
        super::shutdown_tests::run_local(async move {
            let registry = Rc::new(RefCell::new(super::WorkerRegistry::default()));
            let (worker_id, cancellation) = registry.borrow_mut().register();
            let mut workers = tokio::task::JoinSet::new();
            let (actor_sender, actor_receiver) = futures::channel::mpsc::unbounded();
            let (retirement_acknowledgements, retirement_ack_receiver) =
                futures::channel::mpsc::unbounded();
            let (retirement_gate_sender, mut retirement_gate_receiver) =
                futures::channel::mpsc::unbounded();
            let actor = tokio::task::spawn_local(super::run_host_actor(
                actor_receiver,
                actor_sender.clone(),
                super::ConcurrentHostState {
                    transport,
                    authority: super::HostAuthority {
                        database_id: repository.recipe.database_id,
                        runtime_id: [23; 16],
                        expiries: Rc::clone(&expiries),
                    },
                    issuer: SystemCredentialIssuer::default(),
                    deletion,
                    application_workers,
                    application_work,
                    admitted_upgrades: BTreeMap::new(),
                    delivering_upgrades: BTreeMap::new(),
                },
                Rc::clone(&registry),
                retirement_ack_receiver,
                retirement_gate_sender,
            ));
            let worker_actor = actor_sender.clone();
            let worker_registry = Rc::clone(&registry);
            let (allow_exit_sender, allow_exit) = futures::channel::oneshot::channel();
            let task = workers.spawn_local(async move {
                cancellation.await.unwrap();
                allow_exit.await.unwrap();
                let mut cleanup_cancellation = futures::future::pending();
                super::close_worker_attachment(
                    &worker_actor,
                    &worker_registry,
                    attachment,
                    worker_id,
                    &mut cleanup_cancellation,
                )
                .await;
                super::actor_worker_exited(&worker_actor, worker_id).await;
                worker_id
            });
            registry.borrow_mut().bind_task(worker_id, task.id());

            let unavailable_body = br#"{"code":"live.unavailable","message":"request rejected"}"#;
            let unavailable = [
                b"HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\nContent-Length: ".as_slice(),
                unavailable_body.len().to_string().as_bytes(),
                b"\r\n\r\n".as_slice(),
                unavailable_body,
            ]
            .concat();

            let duplicate_request = upgrade.clone();
            let upgrade = super::actor_begin(&actor_sender, upgrade, attachment, worker_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retirement_gate_receiver.next().await.unwrap().len(), 0);
            let duplicate = bounded_test_wait(
                super::actor_begin(&actor_sender, duplicate_request, [23; 16], worker_id),
                "duplicate admitted upgrade",
            )
            .await
            .expect("duplicate admitted upgrade request must reach the actor")
            .expect_err("duplicate admitted upgrade must fail closed");
            assert_eq!(duplicate.status, 503);
            super::actor_deliver(&actor_sender, &upgrade, worker_id)
                .await
                .unwrap();

            let encoded = upgrade
                .response()
                .encode_http(TransportLimits::default())
                .unwrap();
            let mut writer = futures::io::Cursor::new(Vec::new());
            let mut delivery_cancellation = futures::future::pending();
            let delivery = super::deliver_websocket_upgrade_response(
                &mut writer,
                &encoded,
                &mut delivery_cancellation,
            )
            .await;
            let delivered_at_instant = super::Instant::now();
            assert_eq!(delivery, Ok(()));
            assert_eq!(writer.into_inner(), encoded);

            let mut cancellation = futures::future::pending();
            let (_, responses, retirement) = super::actor_http(
                &actor_sender,
                HttpConnection::new(TransportLimits::default()),
                resume_request.as_bytes(),
                &mut cancellation,
            )
            .await
            .unwrap();
            assert_eq!(responses, vec![unavailable.clone()]);
            assert!(retirement.is_empty());

            let mut cancellation = futures::future::pending();
            let (_, responses, retirement) = super::actor_http(
                &actor_sender,
                HttpConnection::new(TransportLimits::default()),
                delete_request.as_bytes(),
                &mut cancellation,
            )
            .await
            .unwrap();
            assert_eq!(responses, vec![unavailable.clone()]);
            assert!(retirement.is_empty());

            registry.borrow_mut().cancel(worker_id);
            // The cursor above completed the write and flush, so this models
            // the worker's matching Commit after cancellation was injected.
            let stale_upgrade = upgrade.clone();
            let retirement =
                super::actor_commit(&actor_sender, upgrade, worker_id, delivered_at_instant)
                    .await
                    .expect("a delivered upgrade must commit after cancellation");
            assert!(retirement.is_empty());
            assert!(
                bounded_test_wait(
                    super::actor_deliver(&actor_sender, &stale_upgrade, worker_id),
                    "stale delivery operation",
                )
                .await
                .is_err()
            );
            allow_exit_sender.send(()).unwrap();
            let joined = bounded_test_wait(workers.join_next_with_id(), "committed worker join")
                .await
                .expect("committed worker must be joined");
            assert!(!super::acknowledge_worker_join(&registry, joined));

            let retirement = bounded_test_wait(
                async {
                    loop {
                        let retirement = retirement_gate_receiver
                            .next()
                            .await
                            .expect("closed attachment must publish its retirement gate");
                        if retirement.iter().any(|gate| gate.attachment == attachment) {
                            break retirement;
                        }
                        assert!(retirement.is_empty());
                    }
                },
                "closed attachment retirement gate",
            )
            .await;
            assert_eq!(retirement.len(), 1);
            assert_eq!(retirement[0].attachment, attachment);

            let mut cancellation = futures::future::pending();
            let (_, responses, retirement_before_ack) = super::actor_http(
                &actor_sender,
                HttpConnection::new(TransportLimits::default()),
                delete_request.as_bytes(),
                &mut cancellation,
            )
            .await
            .unwrap();
            assert_eq!(responses, vec![unavailable]);
            assert!(retirement_before_ack.is_empty());

            assert_eq!(
                super::retire_and_join(&retirement_acknowledgements, retirement).await,
                Ok(())
            );

            let mut cancellation = futures::future::pending();
            let (_, responses, retirement) = super::actor_http(
                &actor_sender,
                HttpConnection::new(TransportLimits::default()),
                delete_request.as_bytes(),
                &mut cancellation,
            )
            .await
            .unwrap();
            assert_eq!(responses, vec![b"HTTP/1.1 204 No Content\r\n\r\n".to_vec()]);
            assert!(retirement.is_empty());

            actor_sender
                .unbounded_send(super::ActorCommand::Shutdown)
                .unwrap();
            actor.await.unwrap();
        });
    }

    #[test]
    fn real_worker_exit_during_delivery_aborts_and_preserves_active_upgrade() {
        let repository = worker_repository();
        let session = [51; 16];
        let now = super::system_milliseconds();
        let expires_at = now.saturating_add(60_000);
        let origin = Origin::parse("https://app.example").unwrap();
        let expiries = Rc::new(RefCell::new(BTreeMap::from([(
            SessionId::new(session),
            expires_at,
        )])));
        let host = LiveHost::new(
            Limits::default(),
            SessionBoundary::new(OriginPolicy::new([origin.clone()], []), 60_000),
            Serving::new(orna_serving_v1::Limits::default()).unwrap(),
        )
        .unwrap();
        let mut transport = LiveTransport::new(host, TransportLimits::default()).unwrap();
        let application = PureEvalApplication::from_repository_with_project(
            &repository.recipe.repository,
            repository.recipe.database_id,
            repository.recipe.identity,
            repository.recipe.initial_digest,
            repository.recipe.runtime_owner,
            Rc::clone(&expiries),
            Some(repository.recipe.project.clone()),
            Some(repository.recipe.capture.clone()),
        )
        .unwrap();
        let application_work = transport.application_work_supervisor();
        let application_workers = ApplicationWorkerRegistry::new(repository.recipe.clone());
        let mut deletion = HostDeletion {
            expiries: Rc::clone(&expiries),
            deleted_leases: Rc::new(RefCell::new(BTreeMap::new())),
            application: Rc::new(RefCell::new(Some(application))),
            application_workers: Some(application_workers.clone()),
        };
        let token = futures::executor::block_on(admit_test_session(
            &mut transport,
            &mut deletion,
            now,
            orna_live_v1::SessionMetadata {
                session,
                database: repository.recipe.database_id,
                runtime: [54; 16],
                expires_at,
                subscribe: subscribe_payload(),
            },
        ));
        let mut token_text = String::with_capacity(43);
        const BASE64URL: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        for chunk in token.chunks(3) {
            token_text.push(BASE64URL[(chunk[0] >> 2) as usize] as char);
            token_text.push(
                BASE64URL
                    [(((chunk[0] & 3) << 4) | (chunk.get(1).copied().unwrap_or(0) >> 4)) as usize]
                    as char,
            );
            if chunk.len() > 1 {
                token_text.push(
                    BASE64URL[(((chunk[1] & 15) << 2) | (chunk.get(2).copied().unwrap_or(0) >> 6))
                        as usize] as char,
                );
            }
            if chunk.len() > 2 {
                token_text.push(BASE64URL[(chunk[2] & 63) as usize] as char);
            }
        }
        super::shutdown_tests::run_local(async move {
            let registry = Rc::new(RefCell::new(super::WorkerRegistry::default()));
            let (actor_sender, actor_receiver) = futures::channel::mpsc::unbounded();
            let (retirement_acknowledgements, retirement_acknowledgement_receiver) =
                futures::channel::mpsc::unbounded();
            let (retirement_gate_sender, mut retirement_gates) =
                futures::channel::mpsc::unbounded();
            let actor = tokio::task::spawn_local(super::run_host_actor(
                actor_receiver,
                actor_sender.clone(),
                super::ConcurrentHostState {
                    transport,
                    authority: super::HostAuthority {
                        database_id: repository.recipe.database_id,
                        runtime_id: [54; 16],
                        expiries,
                    },
                    issuer: SystemCredentialIssuer::default(),
                    deletion,
                    application_workers,
                    application_work,
                    admitted_upgrades: BTreeMap::new(),
                    delivering_upgrades: BTreeMap::new(),
                },
                Rc::clone(&registry),
                retirement_acknowledgement_receiver,
                retirement_gate_sender,
            ));

            let socket = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            socket.set_nonblocking(true).unwrap();
            let address = socket.local_addr().unwrap();
            let listener = tokio::net::TcpListener::from_std(socket).unwrap();
            let mut workers = tokio::task::JoinSet::<u64>::new();

            let (first_worker, first_cancellation) = registry.borrow_mut().register();
            let (first_client, first_accept) = tokio::join!(
                bounded_test_wait(
                    tokio::net::TcpStream::connect(address),
                    "first socket connect"
                ),
                bounded_test_wait(listener.accept(), "first socket accept")
            );
            let mut first_client = first_client.expect("first socket connect must succeed");
            let (first_server, _) = first_accept.expect("first socket accept must succeed");
            let first_actor = actor_sender.clone();
            let first_acknowledgements = retirement_acknowledgements.clone();
            let first_registry = Rc::clone(&registry);
            let first_task = workers.spawn_local(async move {
                super::serve_socket_worker(
                    first_server,
                    first_actor.clone(),
                    first_acknowledgements,
                    first_registry,
                    first_worker,
                    first_cancellation,
                )
                .await;
                super::actor_worker_exited(&first_actor, first_worker).await;
                first_worker
            });
            registry
                .borrow_mut()
                .bind_task(first_worker, first_task.id());

            let handshake = format!(
                "GET /orna/live/33333333-3333-3333-3333-333333333333 HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: orna.present.v1\r\nCookie: orna_session={token_text}\r\n\r\n"
            );
            let delete_request = format!(
                "DELETE /orna/session/33333333-3333-3333-3333-333333333333 HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nAuthorization: Bearer {token_text}\r\n\r\n"
            );
            bounded_test_wait(
                first_client.write_all(handshake.as_bytes()),
                "first WebSocket handshake write",
            )
            .await
            .expect("first WebSocket handshake write must succeed");
            let first_response = bounded_test_wait(
                read_http_headers(&mut first_client, None),
                "first WebSocket response headers",
            )
            .await
            .expect("first WebSocket response headers must be readable");
            assert!(first_response.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
            assert!(
                bounded_test_wait(retirement_gates.next(), "first retirement response")
                    .await
                    .expect("first retirement response channel must remain open")
                    .is_empty()
            );

            let (second_worker, second_cancellation) = registry.borrow_mut().register();
            let (mut delivery_gate, entered_receiver) =
                super::DeliveryTestGateGuard::install(second_worker);
            let (second_client, second_accept) = tokio::join!(
                bounded_test_wait(
                    tokio::net::TcpStream::connect(address),
                    "second socket connect"
                ),
                bounded_test_wait(listener.accept(), "second socket accept")
            );
            let mut second_client = second_client.expect("second socket connect must succeed");
            let (second_server, _) = second_accept.expect("second socket accept must succeed");
            let second_actor = actor_sender.clone();
            let second_acknowledgements = retirement_acknowledgements.clone();
            let second_registry = Rc::clone(&registry);
            let second_task = workers.spawn_local(async move {
                super::serve_socket_worker(
                    second_server,
                    second_actor.clone(),
                    second_acknowledgements,
                    second_registry,
                    second_worker,
                    second_cancellation,
                )
                .await;
                super::actor_worker_exited(&second_actor, second_worker).await;
                second_worker
            });
            registry
                .borrow_mut()
                .bind_task(second_worker, second_task.id());
            bounded_test_wait(
                second_client.write_all(handshake.as_bytes()),
                "second WebSocket handshake write",
            )
            .await
            .expect("second WebSocket handshake write must succeed");
            bounded_test_wait(entered_receiver, "delivery gate entry")
                .await
                .expect("delivery gate must be reached after actor_deliver");
            assert_eq!(
                bounded_test_wait(retirement_gates.next(), "second retirement response")
                    .await
                    .expect("second retirement response channel must remain open")
                    .len(),
                0
            );
            assert_eq!(registry.borrow().attachments.len(), 2);

            let unavailable_body = br#"{"code":"live.unavailable","message":"request rejected"}"#;
            let unavailable = [
                b"HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\nContent-Length: ".as_slice(),
                unavailable_body.len().to_string().as_bytes(),
                b"\r\n\r\n".as_slice(),
                unavailable_body,
            ]
            .concat();
            let mut cancellation = futures::future::pending::<()>();
            let (_, responses, retirement) = bounded_test_wait(
                super::actor_http(
                    &actor_sender,
                    HttpConnection::new(TransportLimits::default()),
                    delete_request.as_bytes(),
                    &mut cancellation,
                ),
                "candidate DELETE actor response",
            )
            .await
            .expect("candidate DELETE actor response must succeed");
            assert_eq!(responses, vec![unavailable]);
            assert!(retirement.is_empty());

            // Release the gate and revoke the candidate. The worker observes
            // cancellation, aborts its delivered reservation, and then sends
            // its own WorkerExited command before returning to the JoinSet.
            delivery_gate.release();
            registry.borrow_mut().cancel(second_worker);
            let joined = bounded_test_wait(
                workers.join_next_with_id(),
                "candidate worker JoinSet result",
            )
            .await
            .expect("candidate worker must be joined");
            assert!(!super::acknowledge_worker_join(&registry, joined));
            let retirement = bounded_test_wait(
                async {
                    loop {
                        let retirement = retirement_gates
                            .next()
                            .await
                            .expect("candidate retirement gate must be published");
                        if retirement.is_empty() {
                            continue;
                        }
                        break retirement;
                    }
                },
                "candidate retirement gate",
            )
            .await;
            assert_eq!(retirement.len(), 1);
            assert_eq!(
                bounded_test_wait(
                    super::retire_and_join(&retirement_acknowledgements, retirement),
                    "candidate retirement acknowledgement",
                )
                .await,
                Ok(())
            );

            let mut second_bytes = Vec::new();
            bounded_test_wait(
                second_client.read_to_end(&mut second_bytes),
                "candidate socket EOF",
            )
            .await
            .expect("candidate socket must reach EOF");
            assert!(second_bytes.is_empty());

            // The incumbent completed handshake remains owned and usable
            // after the failed candidate's cancellation and supervisor join.
            bounded_test_wait(
                first_client.write_all(&[0x89, 0x82, 1, 2, 3, 4, b'o' ^ 1, b'k' ^ 2]),
                "incumbent ping write",
            )
            .await
            .expect("incumbent ping write must succeed");
            let mut pong = [0; 4];
            bounded_test_wait(first_client.read_exact(&mut pong), "incumbent pong read")
                .await
                .expect("incumbent pong must be readable");
            assert_eq!(pong, [0x8a, 0x02, b'o', b'k']);

            // The real HTTP worker routes this response through
            // write_http_after_retirement, which joins the incumbent worker
            // and acknowledges its retirement before writing the 204 bytes.
            let (http_worker, http_cancellation) = registry.borrow_mut().register();
            let (http_client, http_accept) = tokio::join!(
                bounded_test_wait(
                    tokio::net::TcpStream::connect(address),
                    "final HTTP socket connect"
                ),
                bounded_test_wait(listener.accept(), "final HTTP socket accept")
            );
            let mut http_client = http_client.expect("final HTTP socket connect must succeed");
            let (http_server, _) = http_accept.expect("final HTTP socket accept must succeed");
            let http_actor = actor_sender.clone();
            let http_acknowledgements = retirement_acknowledgements.clone();
            let http_registry = Rc::clone(&registry);
            let http_task = workers.spawn_local(async move {
                super::serve_socket_worker(
                    http_server,
                    http_actor.clone(),
                    http_acknowledgements,
                    http_registry,
                    http_worker,
                    http_cancellation,
                )
                .await;
                super::actor_worker_exited(&http_actor, http_worker).await;
                http_worker
            });
            registry.borrow_mut().bind_task(http_worker, http_task.id());
            bounded_test_wait(
                http_client.write_all(delete_request.as_bytes()),
                "final DELETE request write",
            )
            .await
            .expect("final DELETE request write must succeed");
            let (header_readiness_sender, header_readiness_receiver) =
                futures::channel::oneshot::channel();
            let final_response_reader = tokio::task::spawn_local(async move {
                let response = bounded_test_wait(
                    read_http_headers(&mut http_client, Some(header_readiness_sender)),
                    "final DELETE response headers",
                )
                .await;
                (http_client, response)
            });
            bounded_test_wait(header_readiness_receiver, "final response reader readiness")
                .await
                .expect("final response reader must enter the header-read path");
            assert!(
                !final_response_reader.is_finished(),
                "final response must wait for incumbent retirement acknowledgement"
            );

            // Model the real host supervisor: the incumbent only releases
            // this response after its actual JoinSet result is consumed and
            // acknowledged by the worker registry.
            let joined = bounded_test_wait(
                workers.join_next_with_id(),
                "incumbent worker JoinSet result",
            )
            .await
            .expect("incumbent worker must be joined");
            assert!(!super::acknowledge_worker_join(&registry, joined));

            let (mut http_client, final_response) =
                bounded_test_wait(final_response_reader, "final DELETE response reader")
                    .await
                    .expect("final DELETE response reader must finish");
            let final_response =
                final_response.expect("final DELETE response headers must be readable");
            assert_eq!(final_response, b"HTTP/1.1 204 No Content\r\n\r\n");

            bounded_test_wait(http_client.shutdown(), "final HTTP client write shutdown")
                .await
                .expect("final HTTP client write shutdown must succeed");
            let joined = bounded_test_wait(
                workers.join_next_with_id(),
                "final HTTP worker JoinSet result",
            )
            .await
            .expect("final HTTP worker must be joined");
            assert!(!super::acknowledge_worker_join(&registry, joined));

            let mut first_bytes = Vec::new();
            bounded_test_wait(
                first_client.read_to_end(&mut first_bytes),
                "incumbent socket EOF",
            )
            .await
            .expect("incumbent socket must reach EOF");
            assert!(first_bytes.is_empty());

            actor_sender.unbounded_send(ActorCommand::Shutdown).unwrap();
            bounded_test_wait(actor, "actor shutdown")
                .await
                .expect("actor must shut down");
        });
    }

    #[test]
    fn concurrent_shutdown_drains_work_before_worker_exit_and_retirement_acknowledgement() {
        let repository = worker_repository();
        super::shutdown_tests::run_local(async move {
            let session = [24; 16];
            let attachment = [25; 16];
            let now = super::system_milliseconds();
            let expires_at = now.saturating_add(60_000);
            let origin = Origin::parse("https://app.example").unwrap();
            let expiries = Rc::new(RefCell::new(BTreeMap::from([(
                SessionId::new(session),
                expires_at,
            )])));
            let host = LiveHost::new(
                Limits::default(),
                SessionBoundary::new(OriginPolicy::new([origin.clone()], []), 60_000),
                Serving::new(orna_serving_v1::Limits::default()).unwrap(),
            )
            .unwrap();
            let mut transport = LiveTransport::new(host, TransportLimits::default()).unwrap();
            let application_work = transport.application_work_supervisor();
            let application = PureEvalApplication::from_repository_with_project(
                &repository.recipe.repository,
                repository.recipe.database_id,
                repository.recipe.identity,
                repository.recipe.initial_digest,
                repository.recipe.runtime_owner,
                Rc::clone(&expiries),
                Some(repository.recipe.project.clone()),
                Some(repository.recipe.capture.clone()),
            )
            .unwrap();
            let application_workers = ApplicationWorkerRegistry::new(repository.recipe.clone());
            let mut deletion = HostDeletion {
                expiries: Rc::clone(&expiries),
                deleted_leases: Rc::new(RefCell::new(BTreeMap::new())),
                application: Rc::new(RefCell::new(Some(application))),
                application_workers: Some(application_workers.clone()),
            };
            let token = admit_test_session(
                &mut transport,
                &mut deletion,
                now,
                orna_live_v1::SessionMetadata {
                    session,
                    database: repository.recipe.database_id,
                    runtime: [27; 16],
                    expires_at,
                    subscribe: subscribe_payload(),
                },
            )
            .await;
            let mut application_lease = application_work.admit(session, [26; 16]).unwrap();
            let application_drained = Arc::new(AtomicBool::new(false));
            let (worker_exit_acknowledged, worker_exit_acknowledgement) =
                futures::channel::oneshot::channel();
            let application_drained_for_task = Arc::clone(&application_drained);
            let application_task = tokio::task::spawn_local(async move {
                application_lease.cancellation().await.unwrap();
                worker_exit_acknowledgement.await.unwrap();
                application_lease.complete();
                application_drained_for_task.store(true, Ordering::Release);
            });
            let mut token_text = String::with_capacity(43);
            const BASE64URL: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
            for chunk in token.chunks(3) {
                token_text.push(BASE64URL[(chunk[0] >> 2) as usize] as char);
                token_text.push(
                    BASE64URL[(((chunk[0] & 3) << 4) | (chunk.get(1).copied().unwrap_or(0) >> 4))
                        as usize] as char,
                );
                if chunk.len() > 1 {
                    token_text.push(
                        BASE64URL[(((chunk[1] & 15) << 2)
                            | (chunk.get(2).copied().unwrap_or(0) >> 6))
                            as usize] as char,
                    );
                }
                if chunk.len() > 2 {
                    token_text.push(BASE64URL[(chunk[2] & 63) as usize] as char);
                }
            }
            let upgrade_request = WireRequest {
                method: "GET".into(),
                path: "/orna/live/18181818-1818-1818-1818-181818181818".into(),
                headers: vec![
                    ("origin".into(), "https://app.example".into()),
                    ("connection".into(), "Upgrade".into()),
                    ("upgrade".into(), "websocket".into()),
                    ("sec-websocket-version".into(), "13".into()),
                    (
                        "sec-websocket-key".into(),
                        "dGhlIHNhbXBsZSBub25jZQ==".into(),
                    ),
                    ("sec-websocket-protocol".into(), "orna.present.v1".into()),
                    ("cookie".into(), format!("orna_session={token_text}")),
                ],
                body: Vec::new(),
            };
            let registry = Rc::new(RefCell::new(super::WorkerRegistry::default()));
            let (worker_id, cancellation) = registry.borrow_mut().register();
            let mut workers = tokio::task::JoinSet::new();
            let mut retirement_tasks = tokio::task::JoinSet::new();
            let (actor_sender, actor_receiver) = futures::channel::mpsc::unbounded();
            let (retirement_acknowledgements, retirement_acknowledgement_receiver) =
                futures::channel::mpsc::unbounded();
            let (retirement_gate_sender, mut actor_retirement_gates) =
                futures::channel::mpsc::unbounded::<super::RetirementGates>();
            let (retirement_gate_bridge, mut retirement_gates) =
                futures::channel::mpsc::unbounded();
            let (retirement_forwarded, retirement_forwarded_signal) =
                futures::channel::oneshot::channel();
            let observed_retirement_gate = Arc::new(AtomicBool::new(false));
            let observed_retirement_gate_for_bridge = Arc::clone(&observed_retirement_gate);
            let retirement_gate_bridge = tokio::task::spawn_local(async move {
                let mut retirement_forwarded = Some(retirement_forwarded);
                while let Some(retirement) = actor_retirement_gates.next().await {
                    let nonempty = !retirement.is_empty();
                    if retirement_gate_bridge.unbounded_send(retirement).is_err() {
                        break;
                    }
                    if nonempty {
                        observed_retirement_gate_for_bridge.store(true, Ordering::Release);
                        if let Some(retirement_forwarded) = retirement_forwarded.take() {
                            let _ = retirement_forwarded.send(());
                        }
                    }
                }
            });
            let actor = tokio::task::spawn_local(super::run_host_actor(
                actor_receiver,
                actor_sender.clone(),
                super::ConcurrentHostState {
                    transport,
                    authority: super::HostAuthority {
                        database_id: repository.recipe.database_id,
                        runtime_id: [27; 16],
                        expiries,
                    },
                    issuer: SystemCredentialIssuer::default(),
                    deletion,
                    application_workers,
                    application_work,
                    admitted_upgrades: BTreeMap::new(),
                    delivering_upgrades: BTreeMap::new(),
                },
                Rc::clone(&registry),
                retirement_acknowledgement_receiver,
                retirement_gate_sender,
            ));
            let worker_actor = actor_sender.clone();
            let task = workers.spawn_local(async move {
                cancellation.await.unwrap();
                super::actor_worker_exited(&worker_actor, worker_id).await;
                retirement_forwarded_signal.await.unwrap();
                worker_exit_acknowledged.send(()).unwrap();
                worker_id
            });
            registry.borrow_mut().bind_task(worker_id, task.id());

            let upgrade = super::actor_begin(&actor_sender, upgrade_request, attachment, worker_id)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retirement_gates.next().await.unwrap().len(), 0);
            super::actor_deliver(&actor_sender, &upgrade, worker_id)
                .await
                .unwrap();

            assert_eq!(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    super::shutdown_concurrent_host(
                        &registry,
                        &mut workers,
                        &mut retirement_tasks,
                        &mut retirement_gates,
                        &retirement_acknowledgements,
                        actor_sender,
                        Some(actor),
                        super::LiveHostError::Cancelled,
                    ),
                )
                .await,
                Ok(Err(super::LiveHostError::Cancelled))
            );
            application_task.await.unwrap();
            retirement_gate_bridge.await.unwrap();
            assert!(application_drained.load(Ordering::Acquire));
            assert!(observed_retirement_gate.load(Ordering::Acquire));
            assert!(workers.is_empty());
            assert!(retirement_tasks.is_empty());
            assert!(registry.borrow().workers.is_empty());
        });
    }

    struct WorkerRepository {
        root: PathBuf,
        recipe: ApplicationWorkerRecipe,
    }

    impl Drop for WorkerRepository {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn worker_repository() -> WorkerRepository {
        static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/gov5-15-4-12-tmp");
        fs::create_dir_all(&base).unwrap();
        let root = loop {
            let candidate = base.join(format!(
                "live-worker-{}-{}",
                std::process::id(),
                NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("fixture directory: {error}"),
            }
        };
        let initialized = initialize_repository(&root).unwrap();
        let repository = initialized.into_repository();
        let database_id = *inspect_metadata(&repository)
            .unwrap()
            .unwrap()
            .database_id()
            .as_bytes();
        let project = ProjectLoader::default()
            .load_with_standard_profile(
                &repository,
                Some(orna_evaluator_v1::reference_standard_profile()),
            )
            .unwrap();
        let (identity, initial_digest) = runtime_identity(database_id);
        let state =
            futures::executor::block_on(RuntimeState::open(&repository, identity, initial_digest))
                .unwrap();
        let capture = futures::executor::block_on(state.capture()).unwrap();
        WorkerRepository {
            root,
            recipe: ApplicationWorkerRecipe {
                repository,
                project,
                capture,
                database_id,
                identity,
                initial_digest,
                runtime_owner: [44; 16],
                #[cfg(test)]
                eval_started: None,
                #[cfg(test)]
                panic_on_eval: false,
            },
        }
    }

    fn worker_host(
        session: [u8; 16],
        attachment: [u8; 16],
    ) -> (LiveHost, Origin, SessionCredential) {
        let origin = Origin::parse("https://app.example").unwrap();
        let mut host = LiveHost::new(
            Limits::default(),
            SessionBoundary::new(OriginPolicy::new([origin.clone()], []), 60_000),
            Serving::new(orna_serving_v1::Limits::default()).unwrap(),
        )
        .unwrap();
        let mut issuer = orna_live_v1::SystemCredentialIssuer::default();
        let credential = futures::executor::block_on(host.create(
            CreateRequest {
                id: session,
                origin: origin.clone(),
                expires_at: 60_000,
                now: 1,
                subscribe: &subscribe_payload(),
            },
            &mut issuer,
        ))
        .unwrap();
        futures::executor::block_on(host.resume(ResumeRequest {
            id: session,
            origin: &origin,
            credential: &credential,
            attachment,
            now: 2,
        }))
        .unwrap();
        (host, origin, credential)
    }

    fn worker_eval_frame(
        session: [u8; 16],
        request: [u8; 16],
        database: [u8; 16],
        source: &str,
    ) -> Frame {
        let mut envelope = Envelope {
            request: Some(request),
            watch: None,
            message: Message::Eval {
                source: source.into(),
                database: DatabaseContext {
                    database,
                    snapshot: None,
                },
                presentation: PresentationContext {
                    locale: "en-GB".into(),
                    timezone: None,
                    width: None,
                    theme: "terminal/dark".into(),
                    supported_kinds: Vec::new(),
                },
                fingerprint: [0; 32],
            },
            extensions: BTreeMap::new(),
        };
        let fingerprint =
            canonical_request_fingerprint(session, &envelope, orna_protocol_v1::Limits::default())
                .unwrap();
        if let Message::Eval {
            fingerprint: sent, ..
        } = &mut envelope.message
        {
            *sent = fingerprint;
        }
        Frame::Binary(
            envelope
                .encode(orna_protocol_v1::Limits::default())
                .unwrap(),
        )
    }

    fn worker_unsubscribe_frame(request: [u8; 16], watch: [u8; 16]) -> Vec<u8> {
        Envelope {
            request: Some(request),
            watch: Some(watch),
            message: Message::Unsubscribe,
            extensions: BTreeMap::new(),
        }
        .encode(ProtocolLimits::default())
        .expect("static unsubscribe payload")
    }

    fn masked_websocket_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        assert!(payload.len() < 126, "test payload is short");
        let mask = [1, 2, 3, 4];
        let mut frame = vec![u8::from(fin) << 7 | opcode, 128 | payload.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % mask.len()]),
        );
        frame
    }

    fn server_binary_requests(bytes: &[u8]) -> Vec<[u8; 16]> {
        let mut bytes = bytes;
        let mut requests = Vec::new();
        while !bytes.is_empty() {
            assert!(bytes.len() >= 2, "server frame header is complete");
            assert_eq!(bytes[0], 0x82, "server response is a final binary frame");
            let length = match bytes[1] {
                length @ 0..=125 => usize::from(length),
                126 => {
                    assert!(bytes.len() >= 4, "extended server frame header is complete");
                    usize::from(u16::from_be_bytes([bytes[2], bytes[3]]))
                }
                _ => panic!("test response unexpectedly uses a 64-bit length"),
            };
            let header = if bytes[1] <= 125 { 2 } else { 4 };
            assert!(
                bytes.len() >= header + length,
                "server response payload is complete"
            );
            let envelope =
                Envelope::decode(&bytes[header..header + length], ProtocolLimits::default())
                    .expect("server response is canonical");
            assert!(matches!(envelope.message, Message::Result { .. }));
            requests.push(envelope.request.expect("result is correlated"));
            bytes = &bytes[header + length..];
        }
        requests
    }

    #[test]
    fn actor_drains_coalesced_fragmented_messages_in_order() {
        let repository = worker_repository();
        super::shutdown_tests::run_local(async move {
            let session = [41; 16];
            let attachment = [42; 16];
            let now = super::system_milliseconds();
            let origin = Origin::parse("https://app.example").unwrap();
            let mut host = LiveHost::new(
                Limits::default(),
                SessionBoundary::new(OriginPolicy::new([origin.clone()], []), 60_000),
                Serving::new(orna_serving_v1::Limits::default()).unwrap(),
            )
            .unwrap();
            let mut issuer = SystemCredentialIssuer::default();
            let credential = host
                .create(
                    CreateRequest {
                        id: session,
                        origin: origin.clone(),
                        expires_at: now.saturating_add(60_000),
                        now,
                        subscribe: &subscribe_payload(),
                    },
                    &mut issuer,
                )
                .await
                .unwrap();
            host.resume(ResumeRequest {
                id: session,
                origin: &origin,
                credential: &credential,
                attachment,
                now,
            })
            .await
            .unwrap();
            let transport = LiveTransport::new(host, TransportLimits::default()).unwrap();
            let application_work = transport.application_work_supervisor();
            let application_workers = ApplicationWorkerRegistry::new(repository.recipe.clone());
            let expiries = Rc::new(RefCell::new(BTreeMap::from([(
                SessionId::new(session),
                now.saturating_add(60_000),
            )])));
            let deletion = HostDeletion {
                expiries: Rc::clone(&expiries),
                deleted_leases: Rc::new(RefCell::new(BTreeMap::new())),
                application: Rc::new(RefCell::new(None)),
                application_workers: Some(application_workers.clone()),
            };
            let registry = Rc::new(RefCell::new(super::WorkerRegistry::default()));
            let (actor_sender, actor_receiver) = futures::channel::mpsc::unbounded();
            let (_retirement_acknowledgements, retirement_acknowledgement_receiver) =
                futures::channel::mpsc::unbounded();
            let (retirement_gate_sender, _retirement_gates) = futures::channel::mpsc::unbounded();
            let actor = tokio::task::spawn_local(super::run_host_actor(
                actor_receiver,
                actor_sender.clone(),
                super::ConcurrentHostState {
                    transport,
                    authority: super::HostAuthority {
                        database_id: repository.recipe.database_id,
                        runtime_id: [43; 16],
                        expiries,
                    },
                    issuer: SystemCredentialIssuer::default(),
                    deletion,
                    application_workers,
                    application_work,
                    admitted_upgrades: BTreeMap::new(),
                    delivering_upgrades: BTreeMap::new(),
                },
                registry,
                retirement_acknowledgement_receiver,
                retirement_gate_sender,
            ));

            let first_request = [44; 16];
            let second_request = [45; 16];
            let first = worker_unsubscribe_frame(first_request, [46; 16]);
            let second = worker_unsubscribe_frame(second_request, [47; 16]);
            let split = second.len() / 2;
            let mut input = masked_websocket_frame(true, 2, &first);
            input.extend(masked_websocket_frame(false, 2, &second[..split]));
            input.extend(masked_websocket_frame(true, 0, &second[split..]));

            let mut socket = WebSocketState::new(attachment);
            let mut writer = futures::io::Cursor::new(Vec::new());
            let mut cancellation = futures::future::pending();
            assert!(
                !super::serve_websocket_bytes(
                    &actor_sender,
                    &mut socket,
                    &input,
                    &mut writer,
                    &mut cancellation,
                )
                .await
                .unwrap()
            );
            assert_eq!(
                server_binary_requests(writer.get_ref()),
                vec![first_request, second_request],
                "the actor must admit each buffered message once and in wire order"
            );
            assert!(
                !socket
                    .has_complete_frame(TransportLimits::default().max_frame_bytes)
                    .unwrap()
            );

            actor_sender.unbounded_send(ActorCommand::Shutdown).unwrap();
            actor.await.unwrap();
        });
    }

    #[test]
    fn repository_worker_delete_cancels_evaluation_joins_and_fences_state() {
        let repository = worker_repository();
        let session = [31; 16];
        let attachment = [32; 16];
        let (mut host, origin, credential) = worker_host(session, attachment);
        let ticket = match futures::executor::block_on(host.prepare_application_frame(
            attachment,
            3,
            worker_eval_frame(
                session,
                [33; 16],
                repository.recipe.database_id,
                "if true { for value in 1..=1000000000 { value }; let aborted: Int = 1; aborted }",
            ),
        ))
        .unwrap()
        {
            ApplicationPreparation::Work(ticket) => ticket,
            ApplicationPreparation::Completed(_) => panic!("evaluation was not admitted"),
        };
        let started = Arc::new(AtomicBool::new(false));
        let mut worker_recipe = repository.recipe.clone();
        worker_recipe.eval_started = Some(Arc::clone(&started));
        let (reply, _reply_receiver) = futures::channel::oneshot::channel();
        let (completion_sender, mut completions) = futures::channel::mpsc::unbounded();
        let workers = ApplicationWorkerRegistry::new(worker_recipe);
        if workers
            .submit(
                session,
                ApplicationJob {
                    socket: Some(WebSocketState::new(attachment)),
                    ticket: Some(ticket),
                    reply: Some(reply),
                    completion_sender,
                },
            )
            .is_err()
        {
            panic!("bounded worker admission failed");
        }
        while !started.load(Ordering::Acquire) {
            thread::yield_now();
        }

        let expiries = Rc::new(RefCell::new(BTreeMap::from([(
            SessionId::new(session),
            60_000,
        )])));
        let application = PureEvalApplication::from_repository_with_project(
            &repository.recipe.repository,
            repository.recipe.database_id,
            repository.recipe.identity,
            repository.recipe.initial_digest,
            repository.recipe.runtime_owner,
            Rc::clone(&expiries),
            Some(repository.recipe.project.clone()),
            Some(repository.recipe.capture.clone()),
        )
        .unwrap();
        let mut deletion = HostDeletion {
            expiries,
            deleted_leases: Rc::new(RefCell::new(BTreeMap::new())),
            application: Rc::new(RefCell::new(Some(application))),
            application_workers: Some(workers.clone()),
        };
        let mut children = HostApplicationChildren::with_workers(
            host.application_work_supervisor(),
            workers.clone(),
        );
        super::shutdown_tests::run_local(async move {
            assert_eq!(
                host.delete_with_children(
                    orna_live_v1::DeleteRequest {
                        id: session,
                        origin: &origin,
                        credential: &credential,
                        now: 4,
                    },
                    &mut deletion,
                    &mut children,
                )
                .await,
                Ok(())
            );
            assert!(workers.workers.borrow().is_empty());

            let command = completions
                .next()
                .await
                .expect("joined worker must report its fenced completion");
            let ActorCommand::ApplicationComplete {
                completion,
                socket,
                reply,
            } = command
            else {
                panic!("worker reported an unexpected actor command");
            };
            drop(socket);
            drop(reply);
            assert_eq!(
                host.complete_application(completion).await,
                Err(orna_live_v1::Error::Closed)
            );
            assert!(matches!(
                host.prepare_application_frame(
                    attachment,
                    5,
                    worker_eval_frame(session, [34; 16], repository.recipe.database_id, "1"),
                )
                .await,
                Err(orna_live_v1::Error::Closed)
            ));
        });
    }

    #[test]
    fn repository_worker_panic_replies_and_releases_its_join_slot() {
        let repository = worker_repository();
        let session = [35; 16];
        let attachment = [36; 16];
        let (mut host, _origin, _credential) = worker_host(session, attachment);
        let ticket = match futures::executor::block_on(host.prepare_application_frame(
            attachment,
            3,
            worker_eval_frame(session, [37; 16], repository.recipe.database_id, "1"),
        ))
        .unwrap()
        {
            ApplicationPreparation::Work(ticket) => ticket,
            ApplicationPreparation::Completed(_) => panic!("evaluation was not admitted"),
        };
        let mut worker_recipe = repository.recipe.clone();
        worker_recipe.panic_on_eval = true;
        let workers = ApplicationWorkerRegistry::new(worker_recipe);
        let (reply, reply_receiver) = futures::channel::oneshot::channel();
        let (completion_sender, _completions) = futures::channel::mpsc::unbounded();
        if workers
            .submit(
                session,
                ApplicationJob {
                    socket: Some(WebSocketState::new(attachment)),
                    ticket: Some(ticket),
                    reply: Some(reply),
                    completion_sender,
                },
            )
            .is_err()
        {
            panic!("bounded worker admission failed");
        }

        assert!(matches!(
            futures::executor::block_on(reply_receiver),
            Ok(Err(orna_live_v1::Error::ApplicationRejected))
        ));
        workers
            .stop_and_join_blocking(SessionId::new(session))
            .expect("panicked worker must remain joinable");
        assert!(workers.workers.borrow().is_empty());
    }

    #[test]
    fn failed_worker_submission_removes_and_joins_dead_registry_entry() {
        let repository = worker_repository();
        let session = SessionId::new([38; 16]);
        let workers = ApplicationWorkerRegistry::new(repository.recipe.clone());
        let (sender, receiver) = std::sync::mpsc::channel();
        drop(receiver);
        let join = thread::spawn(|| ());
        workers.workers.borrow_mut().insert(
            session,
            super::ApplicationWorkerHandle {
                sender,
                join: Some(join),
            },
        );
        let (completion_sender, _completions) = futures::channel::mpsc::unbounded();
        let (reply, _reply_receiver) = futures::channel::oneshot::channel();
        let job = ApplicationJob {
            socket: None,
            ticket: None,
            reply: Some(reply),
            completion_sender,
        };

        assert!(workers.submit([38; 16], job).is_err());
        assert!(workers.workers.borrow().is_empty());
    }
}
