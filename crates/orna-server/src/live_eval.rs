//! Repository-backed pure Eval/Watch application adapter.
//!
//! The live transport owns request identity, response validation, replay, and
//! the wire-level watch registry. This module owns the semantic/evaluator
//! state that is shared by the local REPL boundary and the executable host.
//! Every session is initialized from one loaded repository project and one
//! runtime CWD capture; a disconnect therefore does not discard a legitimate
//! resumable session, while deletion or expiry removes all application state.

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
use orna_security_v1::SessionId;

/// The HTTP authority and the application share this expiry index. Keeping
/// the index outside either owner lets deletion and the actor's expiry tick
/// remove transport-admitted sessions that never submitted an application
/// request as well as sessions with a retained REPL state.
pub(crate) type SessionExpiries = Rc<RefCell<BTreeMap<SessionId, u64>>>;

struct WatchState {
    source: String,
    revision: u64,
    snapshot: CanonicalSnapshot,
}

struct SessionState {
    repl: AdmittedReplSession,
    presentation: PresentationContext,
    watches: BTreeMap<[u8; 16], WatchState>,
}

/// The context established when the executable host is bound.
///
/// The runtime capture and project source set are admitted together once.
/// Evaluation receives only this immutable bundle and never reads a later
/// worktree or CWD fallback.
struct OperationAdmission {
    repl: AdmittedReplSession,
    snapshot: CanonicalSnapshot,
    presentation: PresentationContext,
}

/// The server's pure Eval/Watch implementation.
pub(crate) struct PureEvalApplication {
    template: AdmittedReplSession,
    database_id: [u8; 16],
    pinned_capture: CwdCapture,
    expiries: SessionExpiries,
    sessions: BTreeMap<SessionId, SessionState>,
}

impl PureEvalApplication {
    /// Builds the per-operation evaluator boundary for one served repository.
    pub(crate) fn from_repository(
        repository: &Repository,
        database_id: [u8; 16],
        capture: CwdCapture,
        expiries: SessionExpiries,
    ) -> std::result::Result<Self, ()> {
        if capture.database_id() != database_id {
            return Err(());
        }
        let project = ProjectLoader::default()
            .load_with_standard_profile(repository, Some(reference_standard_profile()))
            .map_err(|_| ())?;
        let template = AdmittedReplSession::from_loaded_project(
            &project,
            reference_standard_sources(),
            Limits::default(),
        )
        .map_err(|_| ())?;
        Ok(Self {
            template,
            database_id,
            pinned_capture: capture,
            expiries,
            sessions: BTreeMap::new(),
        })
    }

    /// Drops application and authority state whose transport lease has
    /// expired. A disconnected session remains present until this exact
    /// deadline, so a valid resume sees the same REPL bindings and watches.
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

    /// Removes all semantic, evaluator, watch, and expiry state for a
    /// transport deletion. Disconnect/replacement deliberately does not call
    /// this method.
    pub(crate) fn remove(&mut self, session: SessionId) {
        self.expiries.borrow_mut().remove(&session);
        self.sessions.remove(&session);
    }

    fn admit(
        &self,
        database: &DatabaseContext,
        presentation: &PresentationContext,
    ) -> std::result::Result<OperationAdmission, &'static str> {
        if database.database != self.database_id {
            return Err("wire.invalid_message");
        }
        if database
            .snapshot
            .as_ref()
            .is_some_and(|requested| requested != self.pinned_capture.snapshot())
        {
            return Err("wire.snapshot_expired");
        }
        Ok(OperationAdmission {
            repl: self.template.clone(),
            snapshot: self.pinned_capture.snapshot().clone(),
            presentation: presentation.clone(),
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
        let OperationAdmission {
            repl,
            snapshot,
            presentation,
        } = self.admit(database, presentation)?;
        if let std::collections::btree_map::Entry::Vacant(entry) = self.sessions.entry(session) {
            entry.insert(SessionState {
                repl,
                presentation: presentation.clone(),
                watches: BTreeMap::new(),
            });
        }
        let state = self
            .sessions
            .get_mut(&session)
            .expect("session was inserted");
        if state.presentation != presentation {
            return Err("wire.invalid_message");
        }
        Ok((state, snapshot))
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
        let result = {
            let (state, _) = match self.session(session, database, presentation) {
                Ok(state) => state,
                Err(code) => return self.failure(request, *fingerprint, code),
            };
            state.repl.submit(source)
        };
        match result {
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
        }
    }

    fn watch(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> Result<Envelope> {
        let Message::Watch {
            source,
            database,
            presentation,
            refresh_floor,
        } = message
        else {
            return Err(Error::InvalidMessage);
        };
        if refresh_floor.is_some() {
            // This application has no clock scheduler or dependency refresh
            // driver. Rejecting a requested floor keeps a successful Watch
            // honest instead of advertising a subscription that never ticks.
            return Err(Error::ApplicationRejected);
        }
        if !presentation.supported_kinds.is_empty()
            && !presentation
                .supported_kinds
                .iter()
                .any(|kind| kind == "text")
        {
            // The current transport has only a snapshot success channel for
            // Watch. It will turn this application rejection into its stable
            // transport error; successful permitted watches remain live.
            return Err(Error::ApplicationRejected);
        }
        let (watch, value, snapshot) = {
            let (state, snapshot) = self
                .session(session, database, presentation)
                .map_err(|_| Error::ApplicationRejected)?;
            let watch = allocate_watch_id(&state.watches)?;
            let value = state
                .repl
                .preview(source)
                .map_err(|_| Error::ApplicationRejected)?;
            state.watches.insert(
                watch,
                WatchState {
                    source: source.clone(),
                    revision: 0,
                    snapshot: snapshot.clone(),
                },
            );
            (watch, value, snapshot)
        };
        snapshot_envelope(request, watch, 0, value, snapshot)
    }

    fn resync(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        watch: [u8; 16],
        message: &Message,
    ) -> Result<Envelope> {
        if !matches!(message, Message::Resync) {
            return Err(Error::InvalidMessage);
        }
        let session_id = SessionId::new(session);
        if !self.expiries.borrow().contains_key(&session_id) {
            return Err(Error::Closed);
        }
        let (value, revision) = {
            let state = self.sessions.get_mut(&session_id).ok_or(Error::Closed)?;
            let source = state
                .watches
                .get(&watch)
                .ok_or(Error::ApplicationRejected)?
                .source
                .clone();
            let value = state
                .repl
                .preview(&source)
                .map_err(|_| Error::ApplicationRejected)?;
            let watch_state = state
                .watches
                .get_mut(&watch)
                .ok_or(Error::ApplicationRejected)?;
            watch_state.revision = watch_state.revision.saturating_add(1);
            (value, watch_state.revision)
        };
        let snapshot = self
            .sessions
            .get(&session_id)
            .and_then(|state| state.watches.get(&watch))
            .ok_or(Error::ApplicationRejected)?
            .snapshot
            .clone();
        snapshot_envelope(request, watch, revision, value, snapshot)
    }

    fn unsubscribe(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        fingerprint: [u8; 32],
        _: &Message,
    ) -> Result<Envelope> {
        // The transport supplies the authenticated watch handle to its own
        // registry and closes it after this acknowledged unit result. The
        // current application callback omits that handle, so its retained
        // source is bounded by the enclosing session lease and is removed by
        // deletion/expiry cleanup.
        Ok(Envelope {
            request: Some(request),
            watch: None,
            message: Message::Result {
                status: ResultStatus::Success,
                value: Some(orna_foundation_v1::CanonicalValue::unit()),
                fingerprint,
                diagnostic: None,
            },
            extensions: BTreeMap::new(),
        })
    }
}

fn allocate_watch_id(watches: &BTreeMap<[u8; 16], WatchState>) -> Result<[u8; 16]> {
    for _ in 0..8 {
        let mut watch = [0; 16];
        getrandom::fill(&mut watch).map_err(|_| Error::ApplicationRejected)?;
        if watch != [0; 16] && !watches.contains_key(&watch) {
            return Ok(watch);
        }
    }
    Err(Error::ApplicationRejected)
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

fn snapshot_envelope(
    request: [u8; 16],
    watch: [u8; 16],
    revision: u64,
    value: orna_foundation_v1::CanonicalValue,
    snapshot: CanonicalSnapshot,
) -> Result<Envelope> {
    decode_envelope(OvbRaw::Map(vec![
        (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
        (OvbRaw::Int(1.into()), OvbRaw::Int(16.into())),
        (OvbRaw::Int(2.into()), OvbRaw::Bytes(request.to_vec())),
        (OvbRaw::Int(3.into()), OvbRaw::Bytes(watch.to_vec())),
        (
            OvbRaw::Int(4.into()),
            OvbRaw::Map(vec![
                (OvbRaw::Int(0.into()), OvbRaw::Int(revision.into())),
                (
                    OvbRaw::Int(1.into()),
                    OvbRaw::Tag(
                        60012,
                        Box::new(OvbRaw::Array(vec![
                            OvbRaw::Text("text".into()),
                            OvbRaw::Null,
                            OvbRaw::Map(vec![(OvbRaw::Text("value".into()), value.raw().clone())]),
                            OvbRaw::Array(vec![]),
                        ])),
                    ),
                ),
                (OvbRaw::Int(2.into()), snapshot.raw()),
            ]),
        ),
    ]))
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

    fn application() -> (PureEvalApplication, SessionExpiries, [u8; 16]) {
        let database_id = [1; 16];
        let capture = CwdCapture::new(
            CanonicalSnapshot::cwd(database_id, [2; 16], 0.into()).unwrap(),
            [3; 32],
        )
        .unwrap();
        let expiries = Rc::new(RefCell::new(BTreeMap::new()));
        let application = PureEvalApplication {
            template: AdmittedReplSession::new(Limits::default()),
            database_id,
            pinned_capture: capture,
            expiries: Rc::clone(&expiries),
            sessions: BTreeMap::new(),
        };
        (application, expiries, database_id)
    }

    fn eval_message(database_id: [u8; 16], source: &str, fingerprint: [u8; 32]) -> Message {
        Message::Eval {
            source: source.into(),
            database: database(database_id),
            presentation: presentation(),
            fingerprint,
        }
    }

    fn raw_field<'a>(raw: &'a OvbRaw, key: u8) -> &'a OvbRaw {
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
        let diagnostic = raw_field(raw_field(&raw, 4), 3).clone();
        let bytes = Value::new(diagnostic).unwrap().encode().unwrap();
        FoundationDiagnostic::decode_ovb(&bytes).unwrap()
    }

    #[test]
    fn eval_failure_is_a_correlated_structured_diagnostic() {
        let (mut application, expiries, database_id) = application();
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
    fn watch_is_read_only_and_resync_advances_its_revision() {
        let (mut application, expiries, database_id) = application();
        let session = [6; 16];
        expiries.borrow_mut().insert(SessionId::new(session), 100);
        let watch_request = [7; 16];
        let watch = application
            .watch(
                session,
                watch_request,
                &Message::Watch {
                    source: "1 + 1".into(),
                    database: database(database_id),
                    presentation: presentation(),
                    refresh_floor: None,
                },
            )
            .unwrap();
        assert!(matches!(
            watch.message,
            Message::Snapshot { revision: 0, .. }
        ));
        let watch_id = watch.watch.expect("server-issued watch id");
        assert_ne!(watch_id, [0; 16]);

        let resync = application
            .resync(session, [8; 16], watch_id, &Message::Resync)
            .unwrap();
        assert!(matches!(
            resync.message,
            Message::Snapshot { revision: 1, .. }
        ));
        assert_eq!(resync.watch, Some(watch_id));

        let after_watch = application
            .eval(
                session,
                [15; 16],
                &eval_message(database_id, "$_", [16; 32]),
            )
            .unwrap();
        assert!(matches!(
            after_watch.message,
            Message::Result {
                status: ResultStatus::Failure,
                value: None,
                diagnostic: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn watch_with_refresh_floor_is_rejected_without_a_scheduler() {
        let (mut application, expiries, database_id) = application();
        let session = [27; 16];
        expiries.borrow_mut().insert(SessionId::new(session), 100);
        let result = application.watch(
            session,
            [28; 16],
            &Message::Watch {
                source: "1 + 1".into(),
                database: database(database_id),
                presentation: presentation(),
                refresh_floor: Some(orna_protocol_v1::Duration {
                    floor_seconds: 1.into(),
                    nanosecond: 0,
                }),
            },
        );

        assert!(matches!(result, Err(Error::ApplicationRejected)));
        assert!(application.sessions.is_empty());
    }

    #[test]
    fn sessions_are_isolated_and_context_remains_pinned() {
        let (mut application, expiries, database_id) = application();
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
                status: ResultStatus::Failure,
                value: None,
                diagnostic: Some(_),
                ..
            }
        ));
    }

    #[test]
    fn eval_requires_the_pinned_cwd_snapshot() {
        let (mut application, expiries, database_id) = application();
        let session = [30; 16];
        expiries.borrow_mut().insert(SessionId::new(session), 100);
        let mut current = eval_message(database_id, "1 + 1", [35; 32]);
        {
            let Message::Eval { database, .. } = &mut current else {
                unreachable!();
            };
            database.snapshot = Some(application.pinned_capture.snapshot().clone());
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
        let (mut application, expiries, database_id) = application();
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
        let watch = application
            .watch(
                session,
                [29; 16],
                &Message::Watch {
                    source: "answer + 2".into(),
                    database: database(database_id),
                    presentation: presentation(),
                    refresh_floor: None,
                },
            )
            .unwrap()
            .watch
            .unwrap();
        assert!(
            application.sessions[&session_id]
                .watches
                .contains_key(&watch)
        );
        application.remove(session_id);
        assert!(!application.sessions.contains_key(&session_id));
        assert!(!expiries.borrow().contains_key(&session_id));
    }
}
