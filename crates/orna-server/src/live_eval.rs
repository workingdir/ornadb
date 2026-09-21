//! Repository-backed pure Eval/Watch application adapter.
//!
//! The live transport owns request identity, response validation, replay, and
//! the wire-level watch registry. This module owns the semantic/evaluator
//! state that is shared by the local REPL boundary and the executable host.
//! Each Eval/Watch captures its requested durable CWD at admission. The first
//! admitted operation initializes the resumable session overlay for that pin;
//! deletion or expiry removes all application state.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    pin::Pin,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
};

use futures::Future;
use orna_evaluator_v1::{
    AdmittedReplSession, CancellationToken, Limits, ReplSession, parse_admitted_repl,
    reference_standard_profile, reference_standard_sources,
};
use orna_foundation_v1::{
    CanonicalSnapshot, CanonicalValue, CwdCapture, Diagnostic as FoundationDiagnostic,
    DiagnosticSeverity, OvbRaw, SafeText, Value,
};
use orna_live_v1::{
    Error, LiveApplication, LiveApplicationWorkLease, LiveEvalResponse, LiveEvalTransaction, Result,
    RuntimeActivationContext,
};
use orna_project_v1::ProjectLoader;
use orna_protocol_v1::{
    DatabaseContext, Envelope, Limits as ProtocolLimits, Message, PresentNode, PresentationContext,
    ResultStatus,
};
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};

/// The HTTP authority and the application share this expiry index. Keeping
/// the index outside either owner lets deletion and the actor's expiry tick
/// remove transport-admitted sessions that never submitted an application
/// request as well as sessions with a retained REPL state.
pub(crate) type SessionExpiries = Rc<RefCell<BTreeMap<SessionId, u64>>>;

struct SessionState {
    repl: AdmittedReplSession,
    snapshot: CanonicalSnapshot,
    terminal: BTreeMap<[u8; 16], ([u8; 32], Envelope)>,
    watches: BTreeMap<[u8; 16], String>,
}

/// The immutable durable CWD pin captured for one admitted operation.
struct OperationAdmission {
    capture: CwdCapture,
    snapshot: CanonicalSnapshot,
}

/// Server-owned source for the durable CWD pin and immutable project inputs.
///
/// The live transport has already made the request reservation when this is
/// called. Keeping this seam here makes the application obtain its context
/// asynchronously at operation admission, rather than comparing with a
/// capture made when the listener was bound.
trait OperationAdmissionSource {
    fn capture<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<CwdCapture, &'static str>> + 'a>>;
    fn project_matches_capture(&self, capture: &CwdCapture) -> bool;
    fn repl(&self) -> std::result::Result<AdmittedReplSession, &'static str>;
}

struct RepositoryAdmissionSource {
    repository: Repository,
    identity: RuntimeIdentity,
    initial_digest: [u8; 32],
    project: Option<orna_project_v1::LoadedProject>,
    project_capture: Option<CwdCapture>,
}

impl OperationAdmissionSource for RepositoryAdmissionSource {
    fn capture<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<CwdCapture, &'static str>> + 'a>> {
        Box::pin(async move {
            let state = RuntimeState::open(&self.repository, self.identity, self.initial_digest)
                .await
                .map_err(|_| "wire.invalid_message")?;
            state.capture().await.map_err(|_| "wire.invalid_message")
        })
    }

    fn project_matches_capture(&self, capture: &CwdCapture) -> bool {
        self.project_capture
            .as_ref()
            .is_none_or(|bound| bound == capture)
    }

    fn repl(&self) -> std::result::Result<AdmittedReplSession, &'static str> {
        let project = match self.project.as_ref() {
            Some(project) => project.clone(),
            None => ProjectLoader::default()
                .load_with_standard_profile(&self.repository, Some(reference_standard_profile()))
                .map_err(|_| "wire.invalid_message")?,
        };
        AdmittedReplSession::from_loaded_project(
            &project,
            reference_standard_sources(),
            Limits::default(),
        )
        .map_err(|_| "wire.invalid_message")
    }
}

/// The asynchronous callback boundary for one authenticated page action.
///
/// The live protocol owns request identity, CWD admission, cancellation and
/// the terminal commit. The action authority owns the opaque handle and typed
/// input, returning the already-staged response/transaction for that action.
pub type ActionFuture<'a> = Pin<Box<dyn Future<Output = Result<LiveEvalResponse>> + 'a>>;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ActionBinding {
    pub session: [u8; 16],
    pub watch: [u8; 16],
    pub page_revision: u64,
    pub action: [u8; 16],
}

pub trait ActionHandler: Send + Sync {
    /// Checks the callback's declared input shape before any activation work.
    /// A rejected value never reaches `activate`, preserving typed action
    /// admission at the opaque-handle boundary.
    fn accepts(&self, value: &CanonicalValue) -> bool;

    fn activate<'a>(
        &'a self,
        binding: ActionBinding,
        request: [u8; 16],
        fingerprint: [u8; 32],
        value: &'a CanonicalValue,
        context: &'a RuntimeActivationContext,
        work: &'a mut LiveApplicationWorkLease,
    ) -> ActionFuture<'a>;
}

pub trait ActionAuthority: Send + Sync {
    fn activate<'a>(
        &'a self,
        binding: ActionBinding,
        request: [u8; 16],
        fingerprint: [u8; 32],
        value: &'a CanonicalValue,
        context: &'a RuntimeActivationContext,
        work: &'a mut LiveApplicationWorkLease,
    ) -> ActionFuture<'a>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionRegistrationError {
    ZeroHandle,
    DuplicateBinding,
}

pub struct ActionAuthorityRegistry {
    handlers: BTreeMap<ActionBinding, Arc<dyn ActionHandler>>,
}

impl ActionAuthorityRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            handlers: BTreeMap::new(),
        }
    }

    pub fn register<H: ActionHandler + 'static>(
        &mut self,
        binding: ActionBinding,
        handler: H,
    ) -> std::result::Result<(), ActionRegistrationError> {
        if binding.action == [0; 16] {
            return Err(ActionRegistrationError::ZeroHandle);
        }
        if self.handlers.contains_key(&binding) {
            return Err(ActionRegistrationError::DuplicateBinding);
        }
        self.handlers.insert(binding, Arc::new(handler));
        Ok(())
    }
}

impl Default for ActionAuthorityRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionAuthority for ActionAuthorityRegistry {
    fn activate<'a>(
        &'a self,
        binding: ActionBinding,
        request: [u8; 16],
        fingerprint: [u8; 32],
        value: &'a CanonicalValue,
        context: &'a RuntimeActivationContext,
        work: &'a mut LiveApplicationWorkLease,
    ) -> ActionFuture<'a> {
        let Some(handler) = self.handlers.get(&binding) else {
            return Box::pin(async { Err(Error::Denied) });
        };
        if !handler.accepts(value) {
            return Box::pin(async { Err(Error::Denied) });
        }
        handler.activate(binding, request, fingerprint, value, context, work)
    }
}

/// Terminal Eval rejections bound to a live session lease: fingerprint and
/// envelope keyed by session and request.
type RejectedTerminals = BTreeMap<SessionId, BTreeMap<[u8; 16], ([u8; 32], Envelope)>>;

pub(crate) struct PureEvalApplication {
    database_id: [u8; 16],
    admissions: Box<dyn OperationAdmissionSource>,
    action_authority: Arc<dyn ActionAuthority>,
    expiries: SessionExpiries,
    sessions: BTreeMap<SessionId, SessionState>,
    /// Terminal Eval rejections that occurred before an evaluator overlay
    /// could be admitted. They are still bound to a live session lease, so an
    /// exact retry cannot re-admit or re-execute against later repository state.
    rejected_terminals: RejectedTerminals,
    #[cfg(test)]
    eval_started: Option<Arc<AtomicBool>>,
    #[cfg(test)]
    panic_on_eval: bool,
}

impl PureEvalApplication {
    /// Activates a session inside its dedicated evaluator worker. The worker
    /// owns this expiry index, so no Rc-backed application state crosses the
    /// OS-thread boundary; transport admission and deletion remain the
    /// authoritative lifetime fences.
    pub(crate) fn activate_worker_session(&mut self, session: [u8; 16]) {
        self.expiries
            .borrow_mut()
            .entry(SessionId::new(session))
            .or_insert(u64::MAX);
    }

    /// Builds the per-operation evaluator boundary for one served repository.
    pub(crate) fn from_repository(
        repository: &Repository,
        database_id: [u8; 16],
        identity: RuntimeIdentity,
        initial_digest: [u8; 32],
        _runtime_owner: [u8; 16],
        expiries: SessionExpiries,
    ) -> std::result::Result<Self, ()> {
        Self::from_repository_with_project(
            repository,
            database_id,
            identity,
            initial_digest,
            _runtime_owner,
            expiries,
            None,
            None,
        )
    }

    /// Builds a worker-owned application from an immutable project snapshot.
    /// The repository is retained only for the durable CWD capture used when
    /// admitting each request; source loading never re-reads it.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_repository_with_project(
        repository: &Repository,
        database_id: [u8; 16],
        identity: RuntimeIdentity,
        initial_digest: [u8; 32],
        runtime_owner: [u8; 16],
        expiries: SessionExpiries,
        project: Option<orna_project_v1::LoadedProject>,
        capture: Option<CwdCapture>,
    ) -> std::result::Result<Self, ()> {
        Self::from_repository_with_project_and_authority(
            repository,
            database_id,
            identity,
            initial_digest,
            runtime_owner,
            expiries,
            project,
            capture,
            Arc::new(ActionAuthorityRegistry::new()),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_repository_with_project_and_authority(
        repository: &Repository,
        database_id: [u8; 16],
        identity: RuntimeIdentity,
        initial_digest: [u8; 32],
        _runtime_owner: [u8; 16],
        expiries: SessionExpiries,
        project: Option<orna_project_v1::LoadedProject>,
        capture: Option<CwdCapture>,
        action_authority: Arc<dyn ActionAuthority>,
    ) -> std::result::Result<Self, ()> {
        if identity.database_id != database_id {
            return Err(());
        }
        Ok(Self {
            database_id,
            admissions: Box::new(RepositoryAdmissionSource {
                repository: repository.clone(),
                identity,
                initial_digest,
                project,
                project_capture: capture,
            }),
            action_authority,
            expiries,
            sessions: BTreeMap::new(),
            rejected_terminals: BTreeMap::new(),
            #[cfg(test)]
            eval_started: None,
            #[cfg(test)]
            panic_on_eval: false,
        })
    }

    /// Drops application and authority state whose transport lease has
    /// expired. A disconnected session remains present until this exact
    /// deadline, so a valid resume sees the same REPL bindings.
    pub(crate) fn expire(&mut self, now: u64) {
        let expired = {
            let expiries = self.expiries.borrow();
            expiries
                .iter()
                .filter_map(|(session, expires_at)| (*expires_at <= now).then_some(*session))
                .collect::<Vec<_>>()
        };
        let mut expiries = self.expiries.borrow_mut();
        for session in expired {
            expiries.remove(&session);
            self.sessions.remove(&session);
            self.rejected_terminals.remove(&session);
        }
    }

    /// Removes all semantic, evaluator, and expiry state for a
    /// transport deletion. Disconnect/replacement deliberately does not call
    /// this method.
    pub(crate) fn remove(&mut self, session: SessionId) {
        self.expiries.borrow_mut().remove(&session);
        self.sessions.remove(&session);
        self.rejected_terminals.remove(&session);
    }

    async fn admit(
        &self,
        database: &DatabaseContext,
        _presentation: &PresentationContext,
    ) -> std::result::Result<OperationAdmission, &'static str> {
        if database.database != self.database_id {
            return Err("wire.invalid_message");
        }
        // REQUEST-1 step 3: capture the current durable CWD only after the
        // transport has reserved this operation. The resulting capture is
        // retained by the operation and cannot be changed by later CWD or
        // repository movement.
        let capture = self.admissions.capture().await?;
        if capture.database_id() != self.database_id {
            return Err("wire.invalid_message");
        }
        if database
            .snapshot
            .as_ref()
            .is_some_and(|requested| requested != capture.snapshot())
        {
            return Err("wire.snapshot_expired");
        }
        Ok(OperationAdmission {
            snapshot: capture.snapshot().clone(),
            capture,
        })
    }

    async fn session(
        &mut self,
        session: [u8; 16],
        database: &DatabaseContext,
        presentation: &PresentationContext,
    ) -> std::result::Result<(&mut SessionState, CanonicalSnapshot), &'static str> {
        let session = SessionId::new(session);
        if !self.expiries.borrow().contains_key(&session) {
            return Err("wire.session_expired");
        }
        let OperationAdmission { capture, snapshot } = self.admit(database, presentation).await?;
        if !self.sessions.contains_key(&session) {
            // The evaluator is constructed only after the durable capture is
            // accepted, then remains the session overlay for that exact pin.
            let repl = self.admissions.repl()?;
            if !self.admissions.project_matches_capture(&capture) {
                // Do not bind loaded source to a CWD that moved after the
                // immutable evaluator was built. A later repository change
                // cannot affect `repl`, which owns the loaded source.
                return Err("wire.snapshot_expired");
            }
            self.sessions.insert(
                session,
                SessionState {
                    repl,
                    snapshot: snapshot.clone(),
                    terminal: BTreeMap::new(),
                    watches: BTreeMap::new(),
                },
            );
        }
        let state = self
            .sessions
            .get_mut(&session)
            .expect("session was inserted");
        if state.snapshot != snapshot {
            // A resumable REPL overlay cannot be safely transplanted onto a
            // different CWD generation. The next operation was still fully
            // admitted against the durable current capture, but it must not
            // mutate the overlay pinned by an earlier operation.
            return Err("wire.snapshot_expired");
        }
        Ok((state, snapshot))
    }

    fn replay(
        &self,
        session: SessionId,
        request: [u8; 16],
        fingerprint: [u8; 32],
    ) -> Result<Option<Envelope>> {
        let Some((stored, response)) = self
            .sessions
            .get(&session)
            .and_then(|state| state.terminal.get(&request))
            .or_else(|| {
                self.rejected_terminals
                    .get(&session)
                    .and_then(|terminals| terminals.get(&request))
            })
        else {
            return Ok(None);
        };
        if *stored != fingerprint {
            // Request identity is scoped by the authenticated session and
            // binds its canonical operation fingerprint. This callback fence
            // must preserve that rule even if a caller reaches the adapter
            // without the transport's earlier request-record check.
            return Err(Error::RequestMismatch);
        }
        Ok(Some(response.clone()))
    }

    fn retain_terminal(
        &mut self,
        session: SessionId,
        request: [u8; 16],
        fingerprint: [u8; 32],
        response: &Envelope,
    ) {
        if let Some(state) = self.sessions.get_mut(&session) {
            state
                .terminal
                .insert(request, (fingerprint, response.clone()));
        } else if self.expiries.borrow().contains_key(&session) {
            self.rejected_terminals
                .entry(session)
                .or_default()
                .insert(request, (fingerprint, response.clone()));
        }
    }

    fn failure(&self, request: [u8; 16], fingerprint: [u8; 32], code: &str) -> Result<Envelope> {
        let diagnostic = diagnostic_for_code(code)?;
        self.failure_diagnostic(request, fingerprint, diagnostic)
    }

    fn failure_diagnostic(
        &self,
        request: [u8; 16],
        fingerprint: [u8; 32],
        diagnostic: FoundationDiagnostic,
    ) -> Result<Envelope> {
        let diagnostic = diagnostic_raw(diagnostic.with_reference(request))?;
        decode_envelope(OvbRaw::Map(vec![
            (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
            (OvbRaw::Int(1.into()), OvbRaw::Int(18.into())),
            (OvbRaw::Int(2.into()), OvbRaw::Bytes(request.to_vec())),
            (OvbRaw::Int(3.into()), OvbRaw::Null),
            (
                OvbRaw::Int(4.into()),
                OvbRaw::Map(vec![
                    (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
                    (OvbRaw::Int(1.into()), OvbRaw::Null),
                    (OvbRaw::Int(2.into()), OvbRaw::Bytes(fingerprint.to_vec())),
                    (OvbRaw::Int(3.into()), diagnostic),
                ]),
            ),
        ]))
    }
}

impl LiveApplication for PureEvalApplication {
    fn eval(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> Result<Envelope> {
        futures::executor::block_on(
            self.eval_with_cancellation(session, request, message, None, None),
        )
    }

    fn dispatch_event_with_work<'a>(
        &'a mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &'a Message,
        watch: Option<[u8; 16]>,
        fingerprint: [u8; 32],
        context: Option<&'a RuntimeActivationContext>,
        work: &'a mut LiveApplicationWorkLease,
    ) -> Pin<Box<dyn Future<Output = Result<LiveEvalResponse>> + 'a>> {
        let Message::Event {
            revision,
            action,
            value,
            fingerprint: message_fingerprint,
        } = message
        else {
            return Box::pin(async { Err(Error::InvalidMessage) });
        };
        if *message_fingerprint != fingerprint {
            return Box::pin(async { Err(Error::RequestMismatch) });
        }
        let Some(watch) = watch else {
            return Box::pin(async { Err(Error::Denied) });
        };
        let Some(context) = context else {
            return Box::pin(async { Err(Error::UnsupportedOperation) });
        };
        let authority = Arc::clone(&self.action_authority);
        let binding = ActionBinding {
            session,
            watch,
            page_revision: *revision,
            action: *action,
        };
        Box::pin(async move {
            work.check_active()?;
            let response = authority
                .activate(
                    binding,
                    request,
                    fingerprint,
                    value,
                    context,
                    work,
                )
                .await;
            work.complete();
            let response = response?;
            work.check_active()?;
            if !matches!(response, LiveEvalResponse::Transaction { .. }) {
                return Err(Error::ApplicationRejected);
            }
            Ok(response)
        })
    }

    fn dispatch_with_work<'a>(
        &'a mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &'a Message,
        watch: Option<[u8; 16]>,
        fingerprint: [u8; 32],
        work: &'a mut orna_live_v1::LiveApplicationWorkLease,
    ) -> Pin<Box<dyn Future<Output = Result<Envelope>> + 'a>> {
        #[cfg(test)]
        if let Some(started) = &self.eval_started {
            started.store(true, std::sync::atomic::Ordering::Release);
        }
        #[cfg(test)]
        if self.panic_on_eval {
            panic!("test evaluator panic is owned by the application worker");
        }
        let cancellation = CancellationToken::from_shared(work.cancellation_flag());
        Box::pin(async move {
            work.check_active()?;
            let response = match message {
                Message::Eval { .. } => {
                    self.eval_with_cancellation(
                        session,
                        request,
                        message,
                        Some(fingerprint),
                        Some(&cancellation),
                    )
                    .await
                }
                Message::Subscribe { .. } => self.subscribe(session, request, message),
                Message::Resync => self.resync(
                    session,
                    request,
                    watch.ok_or(Error::InvalidMessage)?,
                    message,
                ),
                Message::Unsubscribe => self.unsubscribe(session, request, fingerprint, message),
                Message::Event { .. } => self.event(session, request, message),
                Message::Watch { .. } => self.watch(session, request, message),
                Message::Cancel { .. } => self.cancel(session, request, fingerprint, message),
                _ => Err(Error::InvalidMessage),
            };
            work.complete();
            let response = response?;
            work.check_active()?;
            Ok(response)
        })
    }

    fn watch(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> Result<Envelope> {
        let Message::Watch { .. } = message else {
            return Err(Error::InvalidMessage);
        };
        futures::executor::block_on(self.watch_with_snapshot(session, request, message))
    }

    fn resync(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        watch: [u8; 16],
        _: &Message,
    ) -> Result<Envelope> {
        futures::executor::block_on(self.resync_snapshot(session, request, watch))
    }
}

impl PureEvalApplication {
    async fn watch_with_snapshot(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> Result<Envelope> {
        let Message::Watch {
            source,
            database,
            presentation,
            ..
        } = message
        else {
            return Err(Error::InvalidMessage);
        };
        if let Err(code) = self.session(session, database, presentation).await {
            return self
                .watch_diagnostic(request, diagnostic_for_code(code)?.with_reference(request));
        }
        let (value, snapshot) = match self.preview_session(session, source).await {
            Ok(result) => result,
            Err(error) => {
                return self.watch_diagnostic(request, (*error).with_reference(request));
            }
        };
        let watch = self.allocate_watch(session, source.clone())?;
        let present = PresentNode::from_value(value).map_err(|_| Error::ApplicationRejected)?;
        Ok(Envelope {
            request: Some(request),
            watch: Some(watch),
            message: Message::Snapshot {
                revision: 0,
                present,
                snapshot,
            },
            extensions: BTreeMap::new(),
        })
    }

    async fn resync_snapshot(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        watch: [u8; 16],
    ) -> Result<Envelope> {
        let session_id = SessionId::new(session);
        let source = self
            .sessions
            .get(&session_id)
            .and_then(|state| state.watches.get(&watch))
            .cloned()
            .ok_or(Error::Denied)?;
        let (value, snapshot) = match self.preview_session(session, &source).await {
            Ok(result) => result,
            Err(error) => {
                return self.watch_diagnostic(request, (*error).with_reference(request));
            }
        };
        let present = PresentNode::from_value(value).map_err(|_| Error::ApplicationRejected)?;
        Ok(Envelope {
            request: Some(request),
            watch: Some(watch),
            message: Message::Snapshot {
                revision: 0,
                present,
                snapshot,
            },
            extensions: BTreeMap::new(),
        })
    }

    async fn preview_session(
        &mut self,
        session: [u8; 16],
        source: &str,
    ) -> std::result::Result<
        (orna_foundation_v1::CanonicalValue, CanonicalSnapshot),
        Box<FoundationDiagnostic>,
    > {
        let session_id = SessionId::new(session);
        let state = self.sessions.get_mut(&session_id).ok_or_else(|| {
            diagnostic_for_code("wire.session_expired").expect("stable diagnostic")
        })?;
        let snapshot = state.snapshot.clone();
        let value = state
            .repl
            .preview(source)
            .map_err(|error| Box::new(error.diagnostic().clone()))?;
        Ok((value, snapshot))
    }

    fn watch_diagnostic(
        &self,
        request: [u8; 16],
        diagnostic: FoundationDiagnostic,
    ) -> Result<Envelope> {
        let diagnostic = diagnostic_raw(diagnostic)?;
        decode_envelope(OvbRaw::Map(vec![
            (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
            (OvbRaw::Int(1.into()), OvbRaw::Int(19.into())),
            (OvbRaw::Int(2.into()), OvbRaw::Bytes(request.to_vec())),
            (OvbRaw::Int(3.into()), OvbRaw::Null),
            (
                OvbRaw::Int(4.into()),
                OvbRaw::Map(vec![(OvbRaw::Int(0.into()), diagnostic)]),
            ),
        ]))
    }

    fn allocate_watch(&mut self, session: [u8; 16], source: String) -> Result<[u8; 16]> {
        let session_id = SessionId::new(session);
        let state = self.sessions.get_mut(&session_id).ok_or(Error::Denied)?;
        for _ in 0..8 {
            let mut watch = [0; 16];
            getrandom::fill(&mut watch).map_err(|_| Error::RuntimeUnavailable)?;
            if watch != [0; 16] && !state.watches.contains_key(&watch) {
                state.watches.insert(watch, source);
                return Ok(watch);
            }
        }
        Err(Error::Limit)
    }
    async fn eval_with_cancellation(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &Message,
        admitted_fingerprint: Option<[u8; 32]>,
        cancellation: Option<&CancellationToken>,
    ) -> Result<Envelope> {
        let Message::Eval {
            source,
            database,
            presentation,
            fingerprint,
        } = message
        else {
            return Err(Error::InvalidMessage);
        };
        let operation_fingerprint = admitted_fingerprint.unwrap_or(*fingerprint);
        if operation_fingerprint != *fingerprint {
            return Err(Error::RequestMismatch);
        }
        let session_id = SessionId::new(session);
        if let Some(response) = self.replay(session_id, request, operation_fingerprint)? {
            // The durable transport normally returns a terminal outcome
            // before reaching this callback. This local fence covers a
            // duplicate callback during the same retained session without
            // re-admitting or re-executing its source.
            return Ok(response);
        }
        let result = {
            let (state, _) = match self.session(session, database, presentation).await {
                Ok(state) => state,
                Err(code) => {
                    let response = self.failure(request, operation_fingerprint, code)?;
                    // A session that was already admitted can retain this
                    // terminal rejection just like an evaluator failure. The
                    // matching request must replay the original outcome
                    // rather than re-admitting against a later CWD capture.
                    self.retain_terminal(session_id, request, operation_fingerprint, &response);
                    return Ok(response);
                }
            };
            match cancellation {
                Some(cancellation) => {
                    let input = match parse_admitted_repl(source, Limits::default()) {
                        Ok(input) => input,
                        Err(error) => {
                            let response = self.failure_diagnostic(
                                request,
                                operation_fingerprint,
                                error.diagnostic().clone(),
                            )?;
                            self.retain_terminal(
                                session_id,
                                request,
                                operation_fingerprint,
                                &response,
                            );
                            return Ok(response);
                        }
                    };
                    state
                        .repl
                        .submit_admitted_with_cancellation(&input, cancellation)
                }
                None => state.repl.submit(source),
            }
        };
        let response = match result {
            Ok(Some(value)) => Ok(Envelope {
                request: Some(request),
                watch: None,
                message: Message::Result {
                    status: ResultStatus::Success,
                    value: Some(value),
                    fingerprint: operation_fingerprint,
                    diagnostic: None,
                },
                extensions: BTreeMap::new(),
            }),
            Ok(None) => Ok(Envelope {
                request: Some(request),
                watch: None,
                message: Message::Result {
                    status: ResultStatus::RetainedWithoutValue,
                    value: None,
                    fingerprint: operation_fingerprint,
                    diagnostic: None,
                },
                extensions: BTreeMap::new(),
            }),
            Err(error) => {
                self.failure_diagnostic(request, operation_fingerprint, error.diagnostic().clone())
            }
        }?;
        self.retain_terminal(session_id, request, operation_fingerprint, &response);
        Ok(response)
    }

    #[cfg(test)]
    pub(crate) fn set_test_controls(
        &mut self,
        started: Option<Arc<AtomicBool>>,
        panic_on_eval: bool,
    ) {
        self.eval_started = started;
        self.panic_on_eval = panic_on_eval;
    }
}

fn diagnostic_for_code(code: &str) -> Result<FoundationDiagnostic> {
    let code = SafeText::new(code.to_owned()).map_err(|_| Error::ApplicationRejected)?;
    FoundationDiagnostic::new(code, DiagnosticSeverity::Error, SafeText::redacted())
        .map(|diagnostic| diagnostic.redacted())
        .map_err(|_| Error::ApplicationRejected)
}

fn diagnostic_raw(diagnostic: FoundationDiagnostic) -> Result<OvbRaw> {
    let bytes = diagnostic
        .encode_ovb()
        .map_err(|_| Error::ApplicationRejected)?;
    Ok(Value::decode(&bytes)
        .map_err(|_| Error::ApplicationRejected)?
        .raw()
        .clone())
}

/// The public protocol deliberately keeps diagnostic and PresentNode
/// constructors opaque. This server-only adapter reuses the normative OVB
/// representation and immediately decodes it through the protocol validator,
/// so no unchecked wire node crosses the transport boundary.
fn decode_envelope(raw: OvbRaw) -> Result<Envelope> {
    let bytes = Value::new(raw)
        .map_err(|_| Error::ApplicationRejected)?
        .encode()
        .map_err(|_| Error::ApplicationRejected)?;
    Envelope::decode(&bytes, ProtocolLimits::default()).map_err(|_| Error::ApplicationRejected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        fs,
        path::PathBuf,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        thread,
    };
    use orna_runtime_v1::{NoFault, TableMutation};
    struct TestAdmissionSource {
        capture: Rc<RefCell<CwdCapture>>,
        capture_admissions: Rc<Cell<usize>>,
        repl_admissions: Rc<Cell<usize>>,
    }

    impl OperationAdmissionSource for TestAdmissionSource {
        fn capture<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = std::result::Result<CwdCapture, &'static str>> + 'a>>
        {
            Box::pin(async move {
                self.capture_admissions
                    .set(self.capture_admissions.get() + 1);
                Ok(self.capture.borrow().clone())
            })
        }

        fn project_matches_capture(&self, _capture: &CwdCapture) -> bool {
            true
        }

        fn repl(&self) -> std::result::Result<AdmittedReplSession, &'static str> {
            self.repl_admissions.set(self.repl_admissions.get() + 1);
            Ok(AdmittedReplSession::new(Limits::default()))
        }
    }

    fn presentation() -> PresentationContext {
        PresentationContext {
            locale: "en-GB".into(),
            timezone: None,
            width: None,
            theme: "terminal/dark".into(),
            supported_kinds: Vec::new(),
        }
    }

    fn database(database: [u8; 16]) -> DatabaseContext {
        DatabaseContext {
            database,
            snapshot: None,
        }
    }

    fn capture(database_id: [u8; 16], generation: i64) -> CwdCapture {
        CwdCapture::new(
            CanonicalSnapshot::cwd(database_id, [2; 16], generation.into()).unwrap(),
            [3; 32],
        )
        .unwrap()
    }

    type TestApplication = (
        PureEvalApplication,
        SessionExpiries,
        [u8; 16],
        Rc<RefCell<CwdCapture>>,
        Rc<Cell<usize>>,
        Rc<Cell<usize>>,
    );

    fn application() -> TestApplication {
        let database_id = [1; 16];
        let capture = Rc::new(RefCell::new(capture(database_id, 0)));
        let capture_admissions = Rc::new(Cell::new(0));
        let repl_admissions = Rc::new(Cell::new(0));
        let expiries = Rc::new(RefCell::new(BTreeMap::new()));
        let application = PureEvalApplication {
            database_id,
            admissions: Box::new(TestAdmissionSource {
                capture: Rc::clone(&capture),
                capture_admissions: Rc::clone(&capture_admissions),
                repl_admissions: Rc::clone(&repl_admissions),
            }),
            action_authority: Arc::new(ActionAuthorityRegistry::new()),
            expiries: Rc::clone(&expiries),
            sessions: BTreeMap::new(),
            rejected_terminals: BTreeMap::new(),
            #[cfg(test)]
            eval_started: None,
            #[cfg(test)]
            panic_on_eval: false,
        };
        (
            application,
            expiries,
            database_id,
            capture,
            capture_admissions,
            repl_admissions,
        )
    }
    fn action_context() -> (PathBuf, RuntimeActivationContext) {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/orna-server-action-tests");
        fs::create_dir_all(&base).unwrap();
        let root = loop {
            let candidate = base.join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("action fixture directory: {error}"),
            }
        };
        let initialized = orna_repository_v1::initialize_repository(&root).unwrap();
        let repository = initialized.into_repository();
        let database_id = *orna_repository_v1::inspect_metadata(&repository)
            .unwrap()
            .unwrap()
            .database_id()
            .as_bytes();
        let mut repository_id = database_id;
        for (index, byte) in repository_id.iter_mut().enumerate() {
            *byte ^= 0x5a_u8.wrapping_add(index as u8);
        }
        if repository_id == [0; 16] {
            repository_id[0] = 1;
        }
        let mut initial_digest = [0; 32];
        initial_digest[..16].copy_from_slice(&database_id);
        initial_digest[16..].copy_from_slice(&repository_id);
        let state = futures::executor::block_on(RuntimeState::open(
            &repository,
            RuntimeIdentity {
                database_id,
                repository_id,
            },
            initial_digest,
        ))
        .unwrap();
        let context = futures::executor::block_on(state.begin_activation()).unwrap();
        (root, context)
    }

    fn eval_message(database_id: [u8; 16], source: &str, fingerprint: [u8; 32]) -> Message {
        Message::Eval {
            source: source.into(),
            database: database(database_id),
            presentation: presentation(),
            fingerprint,
        }
    }

    fn raw_field(raw: &OvbRaw, key: u8) -> &OvbRaw {
        let OvbRaw::Map(fields) = raw else {
            panic!("expected an OVB map");
        };
        fields
            .iter()
            .find_map(|(candidate, value)| {
                (candidate == &OvbRaw::Int(i64::from(key).into())).then_some(value)
            })
            .expect("expected an OVB field")
    }

    fn response_diagnostic(response: &Envelope) -> FoundationDiagnostic {
        let encoded = response.encode(ProtocolLimits::default()).unwrap();
        let raw = Value::decode(&encoded).unwrap().raw().clone();
        let diagnostic_key = match response.message {
            Message::Result { .. } => 3,
            Message::Diagnostic { .. } => 0,
            _ => panic!("expected a diagnostic response"),
        };
        let diagnostic = raw_field(raw_field(&raw, 4), diagnostic_key).clone();
        let bytes = Value::new(diagnostic).unwrap().encode().unwrap();
        FoundationDiagnostic::decode_ovb(&bytes).unwrap()
    }

    #[test]
    fn eval_failure_is_a_correlated_structured_diagnostic() {
        let (mut application, expiries, database_id, _, _, _) = application();
        let session = [3; 16];
        expiries.borrow_mut().insert(SessionId::new(session), 100);
        let request = [4; 16];
        let response = application
            .eval(
                session,
                request,
                &eval_message(database_id, "let broken: Int = \"wrong\";", [5; 32]),
            )
            .unwrap();
        assert_eq!(response.request, Some(request));
        assert!(matches!(
            response.message,
            Message::Result {
                status: ResultStatus::Failure,
                value: None,
                diagnostic: Some(_),
                ..
            }
        ));
        let diagnostic = response_diagnostic(&response);
        assert_eq!(diagnostic.code(), "ORNA-S021-TYPE");
        assert_eq!(diagnostic.message(), "<redacted>");
        let diagnostic_json = serde_json::to_value(&diagnostic).unwrap();
        assert!(diagnostic_json.get("reference").is_some());
        assert!(response.encode(ProtocolLimits::default()).is_ok());
    }

    #[test]
    fn watch_evaluates_a_read_only_snapshot_and_resyncs() {
        let (mut application, expiries, database_id, _, _, _) = application();
        let session = [6; 16];
        expiries.borrow_mut().insert(SessionId::new(session), 100);
        let response = application
            .watch(
                session,
                [7; 16],
                &Message::Watch {
                    source: "1 + 1".into(),
                    database: database(database_id),
                    presentation: presentation(),
                    refresh_floor: None,
                },
            )
            .unwrap();
        let watch = response.watch.expect("watch response must bind a watch");
        assert!(matches!(
            response.message,
            Message::Snapshot { revision: 0, .. }
        ));
        let session_id = SessionId::new(session);
        assert!(
            application
                .sessions
                .get(&session_id)
                .is_some_and(|state| state.watches.contains_key(&watch))
        );
        let resync = application
            .resync(session, [8; 16], watch, &Message::Resync)
            .unwrap();
        assert_eq!(resync.watch, Some(watch));
        assert!(matches!(
            resync.message,
            Message::Snapshot { revision: 0, .. }
        ));
    }

    #[test]
    fn live_lease_cancellation_reaches_the_evaluator_loop() {
        let limits = Limits {
            max_source_bytes: 65_536,
            max_steps: 1_000_000_000,
            max_depth: 64,
            max_collection_items: 20_000_000,
            max_string_bytes: 16_384,
            max_integer_digits: 1_024,
        };
        let mut session = ReplSession::new(limits);
        let input =
            parse_admitted_repl("if true { for value in 1..=10000000 { value }; 0 }", limits)
                .unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let token = CancellationToken::from_shared(Arc::clone(&flag));
        let request = thread::spawn(move || {
            for _ in 0..128 {
                thread::yield_now();
            }
            flag.store(true, Ordering::Release);
        });

        let result = session.submit_admitted_with_cancellation(&input, &token);
        request.join().unwrap();
        assert_eq!(result.unwrap_err().code(), "ORNA-EVAL-CANCELLED");
    }

    #[test]
    fn eval_rejects_client_result_envelopes_without_creating_session_state() {
        let (mut application, expiries, _, _, _, _) = application();
        let session = [8; 16];
        let session_id = SessionId::new(session);
        expiries.borrow_mut().insert(session_id, 100);

        assert_eq!(
            application.eval(
                session,
                [9; 16],
                &Message::Result {
                    status: ResultStatus::Success,
                    value: None,
                    fingerprint: [10; 32],
                    diagnostic: None,
                },
            ),
            Err(Error::InvalidMessage)
        );
        assert!(!application.sessions.contains_key(&session_id));
    }

    #[test]
    fn sessions_are_isolated_and_context_remains_pinned() {
        let (mut application, expiries, database_id, _, _, _) = application();
        let first = [17; 16];
        let second = [18; 16];
        expiries.borrow_mut().insert(SessionId::new(first), 100);
        expiries.borrow_mut().insert(SessionId::new(second), 100);

        application
            .eval(
                first,
                [19; 16],
                &eval_message(database_id, "let private_value: Int = 41;", [20; 32]),
            )
            .unwrap();
        let isolated = application
            .eval(
                second,
                [21; 16],
                &eval_message(database_id, "private_value", [22; 32]),
            )
            .unwrap();
        assert!(matches!(
            isolated.message,
            Message::Result {
                status: ResultStatus::Failure,
                value: None,
                diagnostic: Some(_),
                ..
            }
        ));

        let pinned = CanonicalSnapshot::cwd(database_id, [2; 16], 1.into()).unwrap();
        let mut changed = eval_message(database_id, "1", [23; 32]);
        let Message::Eval { database, .. } = &mut changed else {
            unreachable!();
        };
        database.snapshot = Some(pinned);
        let stale = application.eval(first, [24; 16], &changed).unwrap();
        assert!(matches!(
            stale.message,
            Message::Result {
                status: ResultStatus::Failure,
                value: None,
                diagnostic: Some(_),
                ..
            }
        ));

        let mut changed = eval_message(database_id, "1", [25; 32]);
        let Message::Eval { presentation, .. } = &mut changed else {
            unreachable!();
        };
        presentation.theme = "web/light".into();
        let presentation_change = application.eval(first, [26; 16], &changed).unwrap();
        assert!(matches!(
            presentation_change.message,
            Message::Result {
                status: ResultStatus::Success,
                ..
            }
        ));
    }

    #[test]
    fn null_context_is_captured_at_operation_admission() {
        let (mut application, expiries, database_id, current_capture, _, repl_admissions) =
            application();
        let session = [39; 16];
        expiries.borrow_mut().insert(SessionId::new(session), 100);
        *current_capture.borrow_mut() = capture(database_id, 7);

        let response = application
            .eval(
                session,
                [40; 16],
                &eval_message(database_id, "let answer: Int = 40;", [41; 32]),
            )
            .unwrap();
        assert!(matches!(
            response.message,
            Message::Result {
                status: ResultStatus::RetainedWithoutValue,
                ..
            }
        ));
        assert_eq!(
            application.sessions[&SessionId::new(session)].snapshot,
            current_capture.borrow().snapshot().clone()
        );
        assert_eq!(repl_admissions.get(), 1);
    }

    #[test]
    fn explicit_stale_context_is_rejected_without_overlay_mutation() {
        let (mut application, expiries, database_id, current_capture, _, repl_admissions) =
            application();
        let session = [42; 16];
        expiries.borrow_mut().insert(SessionId::new(session), 100);
        *current_capture.borrow_mut() = capture(database_id, 3);
        let mut stale = eval_message(database_id, "let hidden: Int = 1;", [43; 32]);
        let Message::Eval { database, .. } = &mut stale else {
            unreachable!();
        };
        database.snapshot = Some(capture(database_id, 2).snapshot().clone());

        let response = application.eval(session, [44; 16], &stale).unwrap();
        assert!(matches!(
            response.message,
            Message::Result {
                status: ResultStatus::Failure,
                diagnostic: Some(_),
                ..
            }
        ));
        assert!(application.sessions.is_empty());
        assert_eq!(repl_admissions.get(), 0);
    }

    #[test]
    fn matching_terminal_replay_does_not_readmit_or_reexecute() {
        let (mut application, expiries, database_id, _, _, repl_admissions) = application();
        let session = [45; 16];
        let request = [46; 16];
        let message = eval_message(database_id, "let answer: Int = 40;", [47; 32]);
        expiries.borrow_mut().insert(SessionId::new(session), 100);

        let first = application.eval(session, request, &message).unwrap();
        let replay = application.eval(session, request, &message).unwrap();
        assert_eq!(replay, first);
        assert_eq!(repl_admissions.get(), 1);

        let value = application
            .eval(
                session,
                [48; 16],
                &eval_message(database_id, "answer + 2", [49; 32]),
            )
            .unwrap();
        assert!(matches!(
            value.message,
            Message::Result {
                status: ResultStatus::Success,
                value: Some(_),
                ..
            }
        ));
        assert_eq!(repl_admissions.get(), 1);
    }

    #[test]
    fn mismatched_terminal_replay_is_rejected_without_mutating_the_session() {
        let (mut application, expiries, database_id, _, _, repl_admissions) = application();
        let session = [54; 16];
        let request = [55; 16];
        expiries.borrow_mut().insert(SessionId::new(session), 100);

        application
            .eval(
                session,
                request,
                &eval_message(database_id, "let answer: Int = 40;", [56; 32]),
            )
            .unwrap();
        assert_eq!(repl_admissions.get(), 1);

        assert_eq!(
            application.eval(
                session,
                request,
                &eval_message(database_id, "let answer: Int = 0;", [57; 32]),
            ),
            Err(Error::RequestMismatch)
        );
        assert_eq!(repl_admissions.get(), 1);

        let value = application
            .eval(
                session,
                [58; 16],
                &eval_message(database_id, "answer + 2", [59; 32]),
            )
            .unwrap();
        assert!(matches!(
            value.message,
            Message::Result {
                status: ResultStatus::Success,
                value: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn rejected_eval_replays_its_correlated_terminal_without_readmission() {
        let (mut application, expiries, database_id, current_capture, capture_admissions, _) =
            application();
        let session = [50; 16];
        expiries.borrow_mut().insert(SessionId::new(session), 100);
        application
            .eval(
                session,
                [51; 16],
                &eval_message(database_id, "let retained: Int = 1;", [52; 32]),
            )
            .unwrap();

        let request = [53; 16];
        let fingerprint = [54; 32];
        let mut stale = eval_message(database_id, "retained", fingerprint);
        let Message::Eval { database, .. } = &mut stale else {
            unreachable!();
        };
        database.snapshot = Some(capture(database_id, 9).snapshot().clone());

        let first = application.eval(session, request, &stale).unwrap();
        assert!(matches!(
            first.message,
            Message::Result {
                status: ResultStatus::Failure,
                diagnostic: Some(_),
                ..
            }
        ));
        let captures_after_failure = capture_admissions.get();
        *current_capture.borrow_mut() = capture(database_id, 10);

        let replay = application.eval(session, request, &stale).unwrap();
        assert_eq!(replay, first);
        assert_eq!(capture_admissions.get(), captures_after_failure);
        assert!(
            application.sessions[&SessionId::new(session)]
                .terminal
                .contains_key(&request)
        );
    }

    #[test]
    fn preparse_eval_failure_replays_without_readmission() {
        let (
            mut application,
            expiries,
            database_id,
            current_capture,
            capture_admissions,
            repl_admissions,
        ) = application();
        let session = [60; 16];
        let request = [61; 16];
        let fingerprint = [62; 32];
        expiries.borrow_mut().insert(SessionId::new(session), 100);
        let message = eval_message(database_id, "let broken: Int =", fingerprint);
        let supervisor = orna_live_v1::LiveApplicationWorkSupervisor::new();

        let mismatched_fingerprint = [63; 32];
        let mut work = supervisor.admit(session, request).unwrap();
        assert_eq!(
            futures::executor::block_on(application.dispatch_with_work(
                session,
                request,
                &message,
                None,
                mismatched_fingerprint,
                &mut work,
            )),
            Err(Error::RequestMismatch)
        );
        assert_eq!(capture_admissions.get(), 0);
        assert_eq!(repl_admissions.get(), 0);
        assert!(application.sessions.is_empty());
        assert!(application.rejected_terminals.is_empty());

        let mut work = supervisor.admit(session, request).unwrap();
        let first = futures::executor::block_on(application.dispatch_with_work(
            session,
            request,
            &message,
            None,
            fingerprint,
            &mut work,
        ))
        .unwrap();
        assert!(matches!(
            first.message,
            Message::Result {
                status: ResultStatus::Failure,
                value: None,
                diagnostic: Some(_),
                ..
            }
        ));
        let diagnostic = response_diagnostic(&first);
        assert_eq!(diagnostic.code(), "ORNA-EVAL-PARSE");
        assert_eq!(diagnostic.message(), "<redacted>");
        let captures_after_failure = capture_admissions.get();
        let repls_after_failure = repl_admissions.get();

        *current_capture.borrow_mut() = capture(database_id, 1);
        let mut work = supervisor.admit(session, request).unwrap();
        let replay = futures::executor::block_on(application.dispatch_with_work(
            session,
            request,
            &message,
            None,
            fingerprint,
            &mut work,
        ))
        .unwrap();
        assert_eq!(replay, first);
        assert_eq!(capture_admissions.get(), captures_after_failure);
        assert_eq!(repl_admissions.get(), repls_after_failure);
    }

    #[test]
    fn eval_requires_the_pinned_cwd_snapshot() {
        let (mut application, expiries, database_id, current_capture, _, _) = application();
        let session = [30; 16];
        expiries.borrow_mut().insert(SessionId::new(session), 100);
        let mut current = eval_message(database_id, "1 + 1", [35; 32]);
        {
            let Message::Eval { database, .. } = &mut current else {
                unreachable!();
            };
            database.snapshot = Some(current_capture.borrow().snapshot().clone());
        }
        let admitted = application.eval(session, [36; 16], &current).unwrap();
        assert!(matches!(
            admitted.message,
            Message::Result {
                status: ResultStatus::Success,
                value: Some(_),
                ..
            }
        ));

        if let Message::Eval { database, .. } = &mut current {
            database.snapshot =
                Some(CanonicalSnapshot::cwd(database_id, [4; 16], 1.into()).unwrap());
        }
        let stale = application.eval(session, [37; 16], &current).unwrap();
        assert!(matches!(
            stale.message,
            Message::Result {
                status: ResultStatus::Failure,
                diagnostic: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn deletion_and_expiry_remove_state_but_ordinary_resume_keeps_it() {
        let (mut application, expiries, database_id, _, _, _) = application();
        let session = [9; 16];
        let session_id = SessionId::new(session);
        expiries.borrow_mut().insert(session_id, 100);
        let message = eval_message(database_id, "let answer: Int = 40;", [10; 32]);
        application.eval(session, [11; 16], &message).unwrap();
        assert!(application.sessions.contains_key(&session_id));

        application
            .eval(
                session,
                [12; 16],
                &eval_message(database_id, "answer + 2", [13; 32]),
            )
            .unwrap();
        assert!(application.sessions.contains_key(&session_id));

        application.expire(99);
        assert!(application.sessions.contains_key(&session_id));
        assert!(expiries.borrow().contains_key(&session_id));
        application.expire(100);
        assert!(!application.sessions.contains_key(&session_id));
        assert!(!expiries.borrow().contains_key(&session_id));

        expiries.borrow_mut().insert(session_id, 200);
        application.eval(session, [14; 16], &message).unwrap();
        application.remove(session_id);
        assert!(!application.sessions.contains_key(&session_id));
        assert!(!expiries.borrow().contains_key(&session_id));
    }
    struct CountingActionHandler(Arc<AtomicUsize>);

    impl ActionHandler for CountingActionHandler {
        fn accepts(&self, value: &CanonicalValue) -> bool {
            *value == CanonicalValue::unit()
        }

        fn activate<'a>(
            &'a self,
            _binding: ActionBinding,
            request: [u8; 16],
            fingerprint: [u8; 32],
            _value: &'a CanonicalValue,
            _context: &'a RuntimeActivationContext,
            _work: &'a mut LiveApplicationWorkLease,
        ) -> ActionFuture<'a> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                Ok(LiveEvalResponse::transaction(
                    Envelope {
                        request: Some(request),
                        watch: None,
                        message: Message::Result {
                            status: ResultStatus::Success,
                            value: Some(CanonicalValue::unit()),
                            fingerprint,
                            diagnostic: None,
                        },
                        extensions: BTreeMap::new(),
                    },
                    LiveEvalTransaction::new(
                        vec![
                            TableMutation::new(
                                [9; 16],
                                "action-test",
                                vec![1],
                                Some(vec![2]),
                            )
                            .expect("valid test mutation"),
                        ],
                        [10; 32],
                        Arc::new(NoFault),
                    ),
                ))
            })
        }
    }

    #[test]
    fn action_registry_fences_every_binding_identity_before_callback() {
        let calls = Arc::new(AtomicUsize::new(0));
        let binding = ActionBinding {
            session: [1; 16],
            watch: [2; 16],
            page_revision: 7,
            action: [3; 16],
        };
        let mut registry = ActionAuthorityRegistry::new();
        registry
            .register(binding, CountingActionHandler(Arc::clone(&calls)))
            .unwrap();
        let handler = registry.handlers.get(&binding).expect("registered action");
        assert!(
            !handler.accepts(&CanonicalValue::uuid([9; 16])),
            "incompatible typed input must be rejected before callback"
        );

        for wrong in [
            ActionBinding {
                session: [4; 16],
                ..binding
            },
            ActionBinding {
                watch: [5; 16],
                ..binding
            },
            ActionBinding {
                page_revision: 8,
                ..binding
            },
            ActionBinding {
                action: [6; 16],
                ..binding
            },
        ] {
            assert!(
                registry.handlers.get(&wrong).is_none(),
                "mismatched session/watch/revision/action must not resolve"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    fn dispatch_action(
        application: &mut PureEvalApplication,
        context: &RuntimeActivationContext,
        session: [u8; 16],
        watch: [u8; 16],
        revision: u64,
        value: CanonicalValue,
        request: [u8; 16],
    ) -> Result<LiveEvalResponse> {
        let fingerprint = [8; 32];
        let message = Message::Event {
            revision,
            action: [3; 16],
            value,
            fingerprint,
        };
        let supervisor = orna_live_v1::LiveApplicationWorkSupervisor::new();
        let mut work = supervisor.admit(session, request).unwrap();
        futures::executor::block_on(application.dispatch_event_with_work(
            session,
            request,
            &message,
            Some(watch),
            fingerprint,
            Some(context),
            &mut work,
        ))
    }

    #[test]
    fn dispatch_event_fences_scope_and_typed_input_before_callback() {
        let (mut application, _, _, _, _, _) = application();
        let (root, context) = action_context();
        let calls = Arc::new(AtomicUsize::new(0));
        let binding = ActionBinding {
            session: [1; 16],
            watch: [2; 16],
            page_revision: 7,
            action: [3; 16],
        };
        let mut registry = ActionAuthorityRegistry::new();
        registry
            .register(binding, CountingActionHandler(Arc::clone(&calls)))
            .unwrap();
        application.action_authority = Arc::new(registry);

        for (session, watch, revision, value, request) in [
            ([4; 16], [2; 16], 7, CanonicalValue::unit(), [11; 16]),
            ([1; 16], [5; 16], 7, CanonicalValue::unit(), [12; 16]),
            ([1; 16], [2; 16], 8, CanonicalValue::unit(), [13; 16]),
            ([1; 16], [2; 16], 7, CanonicalValue::uuid([9; 16]), [14; 16]),
        ] {
            assert!(matches!(
                dispatch_action(
                    &mut application,
                    &context,
                    session,
                    watch,
                    revision,
                    value,
                    request,
                ),
                Err(Error::Denied)
            ));
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }

        let result = dispatch_action(
            &mut application,
            &context,
            [1; 16],
            [2; 16],
            7,
            CanonicalValue::unit(),
            [15; 16],
        )
        .unwrap();
        assert!(matches!(result, LiveEvalResponse::Transaction { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
