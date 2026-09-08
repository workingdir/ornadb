//! Repository-backed pure Eval/Watch application adapter.
//!
//! The live transport owns request identity, response validation, replay, and
//! the wire-level watch registry. This module owns the semantic/evaluator
//! state that is shared by the local REPL boundary and the executable host.
//! Each Eval/Watch captures its requested durable CWD at admission. The first
//! admitted operation initializes the resumable session overlay for that pin;
//! deletion or expiry removes all application state.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use orna_evaluator_v1::{
    AdmittedReplSession, Limits, reference_standard_profile, reference_standard_sources,
};
use orna_foundation_v1::{
    CanonicalSnapshot, CwdCapture, Diagnostic as FoundationDiagnostic, DiagnosticSeverity, OvbRaw,
    SafeText, Value,
};
use orna_live_v1::{Error, LiveApplication, Result};
use orna_project_v1::ProjectLoader;
use orna_protocol_v1::{
    DatabaseContext, Envelope, Limits as ProtocolLimits, Message, PresentationContext, ResultStatus,
};
use orna_repository_v1::Repository;
use orna_runtime_v1::{RuntimeIdentity, RuntimeState};
use orna_security_v1::SessionId;

/// The HTTP authority and the application share this expiry index. Keeping
/// the index outside either owner lets deletion and the actor's expiry tick
/// remove transport-admitted sessions that never submitted an application
/// request as well as sessions with a retained REPL state.
pub(crate) type SessionExpiries = Rc<RefCell<BTreeMap<SessionId, u64>>>;

struct SessionState {
    repl: AdmittedReplSession,
    snapshot: CanonicalSnapshot,
    terminal: BTreeMap<[u8; 16], ([u8; 32], Envelope)>,
}

/// The immutable durable CWD pin captured for one admitted operation.
struct OperationAdmission {
    snapshot: CanonicalSnapshot,
}

/// Server-owned source for the durable CWD pin and immutable project inputs.
///
/// The live transport has already made the request reservation when this is
/// called. Keeping this seam here makes the remaining synchronous application
/// callback obtain its context at operation admission, rather than comparing
/// with a capture made when the listener was bound.
trait OperationAdmissionSource {
    fn capture(&self) -> std::result::Result<CwdCapture, &'static str>;
    fn repl(&self) -> std::result::Result<AdmittedReplSession, &'static str>;
}

struct RepositoryAdmissionSource {
    repository: Repository,
    identity: RuntimeIdentity,
    initial_digest: [u8; 32],
}

impl OperationAdmissionSource for RepositoryAdmissionSource {
    fn capture(&self) -> std::result::Result<CwdCapture, &'static str> {
        let state = futures::executor::block_on(RuntimeState::open(
            &self.repository,
            self.identity,
            self.initial_digest,
        ))
        .map_err(|_| "wire.invalid_message")?;
        futures::executor::block_on(state.capture()).map_err(|_| "wire.invalid_message")
    }

    fn repl(&self) -> std::result::Result<AdmittedReplSession, &'static str> {
        let project = ProjectLoader::default()
            .load_with_standard_profile(&self.repository, Some(reference_standard_profile()))
            .map_err(|_| "wire.invalid_message")?;
        AdmittedReplSession::from_loaded_project(
            &project,
            reference_standard_sources(),
            Limits::default(),
        )
        .map_err(|_| "wire.invalid_message")
    }
}

/// The server's pure Eval/Watch implementation.
pub(crate) struct PureEvalApplication {
    database_id: [u8; 16],
    admissions: Box<dyn OperationAdmissionSource>,
    expiries: SessionExpiries,
    sessions: BTreeMap<SessionId, SessionState>,
}

impl PureEvalApplication {
    /// Builds the per-operation evaluator boundary for one served repository.
    pub(crate) fn from_repository(
        repository: &Repository,
        database_id: [u8; 16],
        identity: RuntimeIdentity,
        initial_digest: [u8; 32],
        _runtime_owner: [u8; 16],
        expiries: SessionExpiries,
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
            }),
            expiries,
            sessions: BTreeMap::new(),
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
        }
    }

    /// Removes all semantic, evaluator, and expiry state for a
    /// transport deletion. Disconnect/replacement deliberately does not call
    /// this method.
    pub(crate) fn remove(&mut self, session: SessionId) {
        self.expiries.borrow_mut().remove(&session);
        self.sessions.remove(&session);
    }

    fn admit(
        &self,
        database: &DatabaseContext,
        _presentation: &PresentationContext,
    ) -> std::result::Result<OperationAdmission, &'static str> {
        if database.database != self.database_id {
            return Err("wire.invalid_message");
        }
        // REQUEST-1 step 3: capture the current durable CWD only after the
        // transport has reserved this operation. The resulting capture is
        // retained by the session state and cannot be changed by later CWD
        // or repository movement.
        let capture = self.admissions.capture()?;
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
        })
    }

    fn session(
        &mut self,
        session: [u8; 16],
        database: &DatabaseContext,
        presentation: &PresentationContext,
    ) -> std::result::Result<(&mut SessionState, CanonicalSnapshot), &'static str> {
        let session = SessionId::new(session);
        if !self.expiries.borrow().contains_key(&session) {
            return Err("wire.session_expired");
        }
        let OperationAdmission { snapshot } = self.admit(database, presentation)?;
        if !self.sessions.contains_key(&session) {
            // The evaluator is constructed only after the durable capture is
            // accepted, then remains the session overlay for that exact pin.
            let repl = self.admissions.repl()?;
            if self.admissions.capture()?.snapshot() != &snapshot {
                // Do not bind loaded source to a CWD that moved while the
                // immutable evaluator was being built. A later repository
                // change cannot affect `repl`, which owns the loaded source.
                return Err("wire.snapshot_expired");
            }
            self.sessions.insert(
                session,
                SessionState {
                    repl,
                    snapshot: snapshot.clone(),
                    terminal: BTreeMap::new(),
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
        let Message::Eval {
            source,
            database,
            presentation,
            fingerprint,
        } = message
        else {
            return Err(Error::InvalidMessage);
        };
        let session_id = SessionId::new(session);
        if let Some(response) = self.replay(session_id, request, *fingerprint)? {
            // The durable transport normally returns a terminal outcome
            // before reaching this callback. This local fence covers a
            // duplicate callback during the same retained session without
            // re-admitting or re-executing its source.
            return Ok(response);
        }
        let result = {
            let (state, _) = match self.session(session, database, presentation) {
                Ok(state) => state,
                Err(code) => {
                    let response = self.failure(request, *fingerprint, code)?;
                    // A session that was already admitted can retain this
                    // terminal rejection just like an evaluator failure. The
                    // matching request must replay the original outcome
                    // rather than re-admitting against a later CWD capture.
                    self.retain_terminal(session_id, request, *fingerprint, &response);
                    return Ok(response);
                }
            };
            state.repl.submit(source)
        };
        let response = match result {
            Ok(Some(value)) => Ok(Envelope {
                request: Some(request),
                watch: None,
                message: Message::Result {
                    status: ResultStatus::Success,
                    value: Some(value),
                    fingerprint: *fingerprint,
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
                    fingerprint: *fingerprint,
                    diagnostic: None,
                },
                extensions: BTreeMap::new(),
            }),
            Err(error) => {
                self.failure_diagnostic(request, *fingerprint, error.diagnostic().clone())
            }
        }?;
        self.retain_terminal(session_id, request, *fingerprint, &response);
        Ok(response)
    }

    fn watch(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope> {
        Err(Error::UnsupportedOperation)
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
    use std::cell::Cell;

    struct TestAdmissionSource {
        capture: Rc<RefCell<CwdCapture>>,
        capture_admissions: Rc<Cell<usize>>,
        repl_admissions: Rc<Cell<usize>>,
    }

    impl OperationAdmissionSource for TestAdmissionSource {
        fn capture(&self) -> std::result::Result<CwdCapture, &'static str> {
            self.capture_admissions
                .set(self.capture_admissions.get() + 1);
            Ok(self.capture.borrow().clone())
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

    fn application() -> (
        PureEvalApplication,
        SessionExpiries,
        [u8; 16],
        Rc<RefCell<CwdCapture>>,
        Rc<Cell<usize>>,
        Rc<Cell<usize>>,
    ) {
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
            expiries: Rc::clone(&expiries),
            sessions: BTreeMap::new(),
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
    fn watch_is_explicitly_unsupported_without_creating_session_state() {
        let (mut application, expiries, database_id, _, _, _) = application();
        let session = [6; 16];
        expiries.borrow_mut().insert(SessionId::new(session), 100);
        assert_eq!(
            application.watch(
                session,
                [7; 16],
                &Message::Watch {
                    source: "1 + 1".into(),
                    database: database(database_id),
                    presentation: presentation(),
                    refresh_floor: None,
                },
            ),
            Err(Error::UnsupportedOperation)
        );
        let session_id = SessionId::new(session);
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
}
