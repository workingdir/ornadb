use futures::{
    executor::block_on,
    io::{AsyncRead, AsyncWrite, Cursor},
};
use orna_foundation_v1::{
    CanonicalSnapshot, CanonicalValue, Diagnostic as FoundationDiagnostic, DiagnosticSeverity,
    OvbRaw, SafeText, Value,
};
use orna_live_v1::{
    ActionAuthority, ActionAuthorityRegistry, ActionBinding, ActionFuture, ActionHandler,
    CreateRequest, DeleteRequest, Error, Frame, FrameOutcome, HttpBody, HttpConnection,
    HttpConnectionError, HttpEncodeError, HttpIoError, HttpParseError, Limits, ListenerBindError,
    ListenerExposure, LiveApplication, LiveApplicationWorkLease, LiveApplicationWorkSupervisor,
    LiveCredentialIssuer, LiveEvalResponse, LiveEvalTransaction, LiveHost, LiveListenerAcceptor,
    LiveSessionAuthority, LiveSessionChildren, LiveTransport, ResumeRequest, SUBPROTOCOL,
    SessionCredential, SessionMetadata, TransportLimits, WebSocketApplicationPreparation,
    WebSocketOutput, WebSocketState, WebSocketUpgrade, WireRequest, WireResponse,
    encode_websocket_output, parse_http_request,
};
use orna_protocol_v1::{
    DatabaseContext, Envelope, Message, PresentIdentity, PresentKind, PresentNode,
    PresentPropertyKey, PresentationContext, ResultBody, ResultStatus, TargetKind,
    canonical_request_fingerprint,
};
use orna_repository_v1::Repository;
use orna_runtime_v1::{
    FaultInjector, FaultPoint, RequestIdentity, RequestOwner, RequestState, RunObservationStatus,
    RuntimeActivationContext, RuntimeError, RuntimeIdentity, RuntimeState, TableMutation,
    TerminalOutcome,
};
use orna_security_v1::{
    AttachmentId, BoundaryError, CredentialIssuer, Origin, OriginPolicy, SessionBoundary,
    SessionDeletionAdapter,
};
use orna_serving_v1::{Credential as ServingCredential, Limits as ServingLimits, Serving};
use std::{
    collections::BTreeMap,
    fs,
    future::Future,
    io::{Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
    sync::{
        Arc, mpsc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},

};

const LIVE_RUNTIME_BOUNDARY_FIXTURE: &str =
    include_str!("fixtures/live-runtime-boundary.orna");

fn commit_delivered(
    transport: &mut LiveTransport,
    upgrade: WebSocketUpgrade,
    now: u64,
) -> Result<WireResponse, Error> {
    assert!(transport.begin_websocket_upgrade_delivery(&upgrade, now));
    assert!(transport.finish_websocket_upgrade_delivery(&upgrade));
    block_on(transport.commit_websocket_upgrade(upgrade, now))
}

struct FailFirstWriter {
    writes: usize,
}

struct FailAfterFirstWrite {
    writes: usize,
}

#[derive(Default)]
struct RecordingWriter {
    bytes: Vec<u8>,
    flushes: usize,
    closes: usize,
}

impl AsyncWrite for RecordingWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.bytes.extend_from_slice(bytes);
        std::task::Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.flushes += 1;
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_close(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        self.closes += 1;
        std::task::Poll::Ready(Ok(()))
    }
}

struct PendingReader;

impl AsyncRead for PendingReader {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Pending
    }
}

struct PrefixThenPendingReader {
    prefix: Cursor<Vec<u8>>,
}

impl AsyncRead for PrefixThenPendingReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        bytes: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match std::pin::Pin::new(&mut self.prefix).poll_read(context, bytes) {
            std::task::Poll::Ready(Ok(0)) => std::task::Poll::Pending,
            result => result,
        }
    }
}

struct PendingWriter;

impl AsyncWrite for PendingWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
        _: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        context.waker().wake_by_ref();
        std::task::Poll::Pending
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        context.waker().wake_by_ref();
        std::task::Poll::Pending
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

struct CancelAfterPolls {
    polls: usize,
    ready_after: usize,
}

#[derive(Default)]
struct PendingListenerAcceptor {
    accepts: usize,
}

impl LiveListenerAcceptor for PendingListenerAcceptor {
    type Accept<'a> = std::future::Pending<std::io::Result<TcpStream>>;

    fn accept<'a>(&'a mut self, _: &'a TcpListener) -> Self::Accept<'a> {
        self.accepts += 1;
        std::future::pending()
    }
}

impl std::future::Future for CancelAfterPolls {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.polls += 1;
        if self.polls >= self.ready_after {
            std::task::Poll::Ready(())
        } else {
            context.waker().wake_by_ref();
            std::task::Poll::Pending
        }
    }
}

impl AsyncWrite for FailFirstWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.writes += 1;
        std::task::Poll::Ready(if self.writes == 1 {
            Err(std::io::Error::other("write rejected"))
        } else {
            Ok(0)
        })
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for FailAfterFirstWrite {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        self.writes += 1;
        std::task::Poll::Ready(if self.writes == 1 {
            Ok(bytes.len())
        } else {
            Err(std::io::Error::other("write rejected"))
        })
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

struct CountingAuthority {
    calls: usize,
    times: Vec<u64>,
}

impl LiveSessionAuthority for CountingAuthority {
    fn create_session(&mut self, database: [u8; 16], now: u64) -> Result<SessionMetadata, Error> {
        self.calls += 1;
        self.times.push(now);
        Ok(SessionMetadata {
            session: [u8::try_from(self.calls).expect("test call count fits"); 16],
            database,
            runtime: [3; 16],
            expires_at: now + 100,
            subscribe: subscribe(),
        })
    }
}

struct Issuer(u8, Option<[u8; 32]>);
impl CredentialIssuer for Issuer {
    fn issue_credential(&mut self) -> Result<[u8; 32], BoundaryError> {
        let credential = [self.0; 32];
        self.0 += 1;
        self.1 = Some(credential);
        Ok(credential)
    }
}
impl LiveCredentialIssuer for Issuer {
    fn last_issued(&self) -> Option<[u8; 32]> {
        self.1
    }
}
struct Delete(bool);
impl SessionDeletionAdapter for Delete {
    type Error = ();
    fn delete(&mut self, _: orna_security_v1::SessionId) -> Result<(), Self::Error> {
        self.0.then_some(()).ok_or(())
    }
}

#[derive(Default)]
struct RecordingDelete {
    calls: usize,
}

impl SessionDeletionAdapter for RecordingDelete {
    type Error = ();

    fn delete(&mut self, _: orna_security_v1::SessionId) -> Result<(), Self::Error> {
        self.calls += 1;
        Ok(())
    }
}

#[derive(Default)]
struct RecordingChildren {
    calls: usize,
    requests: Vec<RequestIdentity>,
    fail: bool,
}

impl LiveSessionChildren for RecordingChildren {
    fn cancel_and_join_session<'a>(
        &'a mut self,
        _: [u8; 16],
        requests: &'a [RequestIdentity],
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>> {
        self.calls += 1;
        self.requests = requests.to_vec();
        Box::pin(async move {
            if self.fail {
                Err(Error::ApplicationRejected)
            } else {
                Ok(())
            }
        })
    }
}

struct LateAdmissionChildren<'a> {
    runtime: &'a RuntimeState,
    identity: RequestIdentity,
    fingerprint: [u8; 32],
    calls: usize,
}

impl LiveSessionChildren for LateAdmissionChildren<'_> {
    fn cancel_and_join_session<'a>(
        &'a mut self,
        _: [u8; 16],
        _: &'a [RequestIdentity],
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + 'a>> {
        self.calls += 1;
        Box::pin(async move {
            match self
                .runtime
                .reserve_request(self.identity, self.fingerprint)
                .await
            {
                Err(RuntimeError::SessionClosed) => Ok(()),
                Ok(_) => Err(Error::ApplicationRejected),
                Err(_) => Err(Error::RuntimeUnavailable),
            }
        })
    }
}

#[derive(Clone, Copy, Default)]
enum UnitEvalOutcome {
    #[default]
    Unit,
    SemanticFailure,
}

#[derive(Default)]
struct UnitApplication {
    calls: usize,
    reject: bool,
    reject_cancel: bool,
    eval_outcome: UnitEvalOutcome,
}

struct CompetingTerminalApplication {
    repository: Repository,
    owner: [u8; 16],
    calls: usize,
}

struct FailAt(FaultPoint);

impl FaultInjector for FailAt {
    fn check(&self, point: FaultPoint) -> Result<(), RuntimeError> {
        if point == self.0 {
            Err(RuntimeError::FaultInjected(point))
        } else {
            Ok(())
        }
    }
}

fn unit_result(request: [u8; 16], fingerprint: [u8; 32]) -> Envelope {
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
    }
}

fn semantic_failure_result(request: [u8; 16], fingerprint: [u8; 32]) -> Envelope {
    Envelope {
        request: Some(request),
        watch: None,
        message: Message::Result {
            status: ResultStatus::Failure,
            value: None,
            fingerprint,
            diagnostic: None,
        },
        extensions: BTreeMap::new(),
    }
}

impl LiveApplication for UnitApplication {
    fn eval(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> Result<Envelope, Error> {
        let Message::Eval { fingerprint, .. } = message else {
            return Err(Error::ApplicationRejected);
        };
        self.calls += 1;
        if self.reject {
            return Err(Error::ApplicationRejected);
        }
        Ok(match self.eval_outcome {
            UnitEvalOutcome::Unit => unit_result(request, *fingerprint),
            UnitEvalOutcome::SemanticFailure => semantic_failure_result(request, *fingerprint),
        })
    }

    fn watch(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope, Error> {
        Err(Error::UnsupportedOperation)
    }

    fn event(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> Result<Envelope, Error> {
        let Message::Event { fingerprint, .. } = message else {
            return Err(Error::ApplicationRejected);
        };
        self.calls += 1;
        Ok(unit_result(request, *fingerprint))
    }

    fn cancel(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        fingerprint: [u8; 32],
        _: &Message,
    ) -> Result<Envelope, Error> {
        self.calls += 1;
        if self.reject_cancel {
            return Err(Error::ApplicationRejected);
        }
        Ok(unit_result(request, fingerprint))
    }
}

impl LiveApplication for CompetingTerminalApplication {
    fn eval(
        &mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &Message,
    ) -> Result<Envelope, Error> {
        let Message::Eval { fingerprint, .. } = message else {
            return Err(Error::ApplicationRejected);
        };
        self.calls += 1;
        let malformed_winner = Envelope {
            request: Some([99; 16]),
            watch: None,
            message: Message::Result {
                status: ResultStatus::Success,
                value: Some(CanonicalValue::unit()),
                fingerprint: *fingerprint,
                diagnostic: None,
            },
            extensions: BTreeMap::new(),
        };
        let repository = self.repository.clone();
        let owner = self.owner;
        let fingerprint = *fingerprint;
        std::thread::spawn(move || {
            let runtime = open_durable_state(&repository);
            let writer = block_on(runtime.acquire_lease(owner)).unwrap();
            block_on(
                runtime.complete_observed_request_with_owner(
                    RequestIdentity {
                        session_id: session,
                        request_id: request,
                    },
                    fingerprint,
                    writer,
                    TerminalOutcome::new(
                        malformed_winner.encode(Limits::default().protocol).unwrap(),
                    )
                    .unwrap(),
                ),
            )
            .unwrap();
        })
        .join()
        .unwrap();
        Ok(unit_result(request, fingerprint))
    }
    fn eval_with_transaction<'a>(
        &'a mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &'a Message,
        _: Option<&'a orna_runtime_v1::RuntimeActivationContext>,
        _: &'a mut orna_live_v1::LiveApplicationWorkLease,
    ) -> Pin<Box<dyn Future<Output = Result<LiveEvalResponse, Error>> + 'a>> {
        let Message::Eval { fingerprint, .. } = message else {
            return Box::pin(async { Err(Error::ApplicationRejected) });
        };
        self.calls += 1;
        let malformed_winner = Envelope {
            request: Some([99; 16]),
            watch: None,
            message: Message::Result {
                status: ResultStatus::Success,
                value: Some(CanonicalValue::unit()),
                fingerprint: *fingerprint,
                diagnostic: None,
            },
            extensions: BTreeMap::new(),
        };
        let repository = self.repository.clone();
        let owner = self.owner;
        let fingerprint = *fingerprint;
        std::thread::spawn(move || {
            let runtime = open_durable_state(&repository);
            let writer = block_on(runtime.acquire_lease(owner)).unwrap();
            block_on(
                runtime.complete_observed_request_with_owner(
                    RequestIdentity {
                        session_id: session,
                        request_id: request,
                    },
                    fingerprint,
                    writer,
                    TerminalOutcome::new(
                        malformed_winner.encode(Limits::default().protocol).unwrap(),
                    )
                    .unwrap(),
                ),
            )
            .unwrap();
        })
        .join()
        .unwrap();
        let transaction = LiveEvalTransaction::new(
            vec![TableMutation::new(
                [8; 16],
                "books",
                vec![8],
                Some(vec![9]),
            )
            .unwrap()],
            [4; 32],
            Arc::new(NoFault),
        );
        let response = unit_result(request, fingerprint);
        Box::pin(async move { Ok(LiveEvalResponse::transaction(response, transaction)) })
    }

    fn dispatch_eval_with_work<'a>(
        &'a mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &'a Message,
        context: Option<&'a orna_runtime_v1::RuntimeActivationContext>,
        work: &'a mut orna_live_v1::LiveApplicationWorkLease,
    ) -> Pin<Box<dyn Future<Output = Result<LiveEvalResponse, Error>> + 'a>> {
        Box::pin(async move {
            work.check_active()?;
            let response = self
                .eval_with_transaction(session, request, message, context, work)
                .await?;
            work.complete();
            work.check_active()?;
            Ok(response)
        })
    }

    fn watch(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope, Error> {
        Err(Error::UnsupportedOperation)
    }
}
struct NoFault;

impl FaultInjector for NoFault {
    fn check(&self, _: FaultPoint) -> Result<(), RuntimeError> {
        Ok(())
    }
}

struct TransactionalApplication {
    calls: usize,
    faults: Arc<dyn FaultInjector>,
    mutations: Vec<TableMutation>,
}

impl LiveApplication for TransactionalApplication {
    fn eval(
        &mut self,
        _: [u8; 16],
        _: [u8; 16],
        _: &Message,
    ) -> Result<Envelope, Error> {
        Err(Error::UnsupportedOperation)
    }

    fn eval_with_transaction<'a>(
        &'a mut self,
        _: [u8; 16],
        request: [u8; 16],
        message: &'a Message,
        _: Option<&'a orna_runtime_v1::RuntimeActivationContext>,
        _: &'a mut orna_live_v1::LiveApplicationWorkLease,
    ) -> Pin<Box<dyn Future<Output = Result<LiveEvalResponse, Error>> + 'a>> {
        let Message::Eval { fingerprint, .. } = message else {
            return Box::pin(async { Err(Error::ApplicationRejected) });
        };
        self.calls += 1;
        let response = unit_result(request, *fingerprint);
        let transaction = LiveEvalTransaction::new(
            self.mutations.clone(),
            [3; 32],
            Arc::clone(&self.faults),
        );
        Box::pin(async move { Ok(LiveEvalResponse::transaction(response, transaction)) })
    }
    fn dispatch_eval_with_work<'a>(
        &'a mut self,
        session: [u8; 16],
        request: [u8; 16],
        message: &'a Message,
        context: Option<&'a orna_runtime_v1::RuntimeActivationContext>,
        work: &'a mut orna_live_v1::LiveApplicationWorkLease,
    ) -> Pin<Box<dyn Future<Output = Result<LiveEvalResponse, Error>> + 'a>> {
        Box::pin(async move {
            work.check_active()?;
            let response = self
                .eval_with_transaction(session, request, message, context, work)
                .await?;
            work.complete();
            work.check_active()?;
            Ok(response)
        })
    }

    fn dispatch_event_with_work<'a>(
        &'a mut self,
        _: [u8; 16],
        request: [u8; 16],
        message: &'a Message,
        _: Option<[u8; 16]>,
        _: [u8; 32],
        _: Option<&'a orna_runtime_v1::RuntimeActivationContext>,
        _: &'a mut orna_live_v1::LiveApplicationWorkLease,
    ) -> Pin<Box<dyn Future<Output = Result<LiveEvalResponse, Error>> + 'a>> {
        let Message::Event { fingerprint, .. } = message else {
            return Box::pin(async { Err(Error::ApplicationRejected) });
        };
        self.calls += 1;
        let response = unit_result(request, *fingerprint);
        let transaction = LiveEvalTransaction::new(
            self.mutations.clone(),
            [3; 32],
            Arc::clone(&self.faults),
        );
        Box::pin(async move { Ok(LiveEvalResponse::transaction(response, transaction)) })
    }

    fn subscribe(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        _: &Message,
    ) -> Result<Envelope, Error> {
        Ok(Envelope {
            request: Some(request),
            watch: Some([11; 16]),
            message: Message::Snapshot {
                revision: 0,
                present: orna_protocol_v1::PresentNode::from_value(CanonicalValue::unit())
                    .unwrap(),
                snapshot: CanonicalSnapshot::cwd([2; 16], [3; 16], 0.into()).unwrap(),
            },
            extensions: BTreeMap::new(),
        })
    }

    fn watch(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope, Error> {
        Err(Error::UnsupportedOperation)
    }
}
#[derive(Clone, Copy)]
enum WatchEventMode {
    Pure,
    Rejected,
    Denied,
    Rollback,
    Commit,
}

struct WatchEventApplication {
    mode: WatchEventMode,
    subscriptions: usize,
    events: usize,
}

struct DeferredWatchEventApplication {
    events: usize,
}

struct IdentityWatchApplication {
    next_revision: u64,
    next_present: PresentNode,
}

impl IdentityWatchApplication {
    fn present(label: &str) -> PresentNode {
        let text = |value: &str| CanonicalValue::new(OvbRaw::Text(value.to_owned())).unwrap();
        let child = |kind: &str, identity: Option<PresentIdentity>, value: &str| {
            PresentNode::new(
                PresentKind::Name(kind.to_owned()),
                identity,
                [(PresentPropertyKey::Name("label".into()), text(value))],
                [],
            )
            .unwrap()
        };
        let field = child(
            "record-field",
            Some(PresentIdentity::RecordField("name".into())),
            label,
        );
        let relation = child(
            "relation-row",
            Some(PresentIdentity::RelationRow {
                table_object_id: [41; 16],
                primary_key: text("customer-7"),
            }),
            label,
        );
        let keyed = child(
            "keyed-card",
            Some(PresentIdentity::Explicit(text("summary"))),
            label,
        );
        let positional = child("list-item", None, label);
        PresentNode::new(
            PresentKind::Name("page".into()),
            None,
            [],
            [field, relation, keyed, positional],
        )
        .unwrap()
    }

    fn snapshot(request: [u8; 16], watch: [u8; 16], revision: u64, present: PresentNode) -> Envelope {
        Envelope {
            request: Some(request),
            watch: Some(watch),
            message: Message::Snapshot {
                revision,
                present,
                snapshot: CanonicalSnapshot::cwd([2; 16], [3; 16], 0.into()).unwrap(),
            },
            extensions: BTreeMap::new(),
        }
    }
}

impl LiveApplication for IdentityWatchApplication {
    fn eval(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope, Error> {
        Err(Error::UnsupportedOperation)
    }

    fn watch(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope, Error> {
        Err(Error::UnsupportedOperation)
    }

    fn subscribe(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        _: &Message,
    ) -> Result<Envelope, Error> {
        Ok(Self::snapshot(
            request,
            [55; 16],
            0,
            Self::present("initial"),
        ))
    }

    fn resync(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        watch: [u8; 16],
        _: &Message,
    ) -> Result<Envelope, Error> {
        Ok(Self::snapshot(
            request,
            watch,
            self.next_revision,
            self.next_present.clone(),
        ))
    }
}

impl WatchEventApplication {
    fn snapshot(request: [u8; 16], watch: [u8; 16]) -> Envelope {
        Envelope {
            request: Some(request),
            watch: Some(watch),
            message: Message::Snapshot {
                revision: 0,
                present: orna_protocol_v1::PresentNode::from_value(CanonicalValue::unit())
                    .unwrap(),
                snapshot: CanonicalSnapshot::cwd([2; 16], [3; 16], 0.into()).unwrap(),
            },
            extensions: BTreeMap::new(),
        }
    }
}

impl LiveApplication for WatchEventApplication {
    fn eval(
        &mut self,
        _: [u8; 16],
        _: [u8; 16],
        _: &Message,
    ) -> Result<Envelope, Error> {
        Err(Error::UnsupportedOperation)
    }

    fn watch(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope, Error> {
        Err(Error::UnsupportedOperation)
    }

    fn subscribe(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        _: &Message,
    ) -> Result<Envelope, Error> {
        self.subscriptions += 1;
        Ok(Self::snapshot(request, [10 + self.subscriptions as u8; 16]))
    }

    fn resync(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        watch: [u8; 16],
        _: &Message,
    ) -> Result<Envelope, Error> {
        Ok(Self::snapshot(request, watch))
    }

    fn unsubscribe(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        fingerprint: [u8; 32],
        _: &Message,
    ) -> Result<Envelope, Error> {
        Ok(unit_result(request, fingerprint))
    }

    fn dispatch_event_with_work<'a>(
        &'a mut self,
        _: [u8; 16],
        request: [u8; 16],
        message: &'a Message,
        _: Option<[u8; 16]>,
        _: [u8; 32],
        _: Option<&'a orna_runtime_v1::RuntimeActivationContext>,
        _: &'a mut orna_live_v1::LiveApplicationWorkLease,
    ) -> Pin<Box<dyn Future<Output = Result<LiveEvalResponse, Error>> + 'a>> {
        self.events += 1;
        let Message::Event { fingerprint, .. } = message else {
            return Box::pin(async { Err(Error::ApplicationRejected) });
        };
        let response = unit_result(request, *fingerprint);
        let mode = self.mode;
        Box::pin(async move {
            match mode {
                WatchEventMode::Pure => Ok(LiveEvalResponse::pure(response)),
                WatchEventMode::Rejected => Err(Error::ApplicationRejected),
                WatchEventMode::Denied => Err(Error::Denied),
                WatchEventMode::Rollback => Ok(LiveEvalResponse::transaction(
                    response,
                    LiveEvalTransaction::new(
                        vec![TableMutation::new(
                            [201; 16],
                            "watch_rollback",
                            vec![1],
                            Some(vec![2]),
                        )
                        .unwrap()],
                        [201; 32],
                        Arc::new(FailAt(FaultPoint::AfterTableWrite)),
                    ),
                )),
                WatchEventMode::Commit => Ok(LiveEvalResponse::transaction(
                    response,
                    LiveEvalTransaction::new(
                        vec![TableMutation::new(
                            [202; 16],
                            "watch_commit",
                            vec![1],
                            Some(vec![2]),
                        )
                        .unwrap()],
                        [202; 32],
                        Arc::new(NoFault),
                    ),
                )),
            }
        })
    }
}


struct Authority;
impl LiveSessionAuthority for Authority {
    fn create_session(&mut self, database: [u8; 16], _: u64) -> Result<SessionMetadata, Error> {
        Ok(SessionMetadata {
            session: [1; 16],
            database,
            runtime: [3; 16],
            expires_at: 100,
            subscribe: subscribe(),
        })
    }
}

fn origin() -> Origin {
    Origin::parse("https://app.example").unwrap()
}
fn host() -> LiveHost {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    LiveHost::new(
        Limits::default(),
        boundary,
        Serving::new(ServingLimits::default()).unwrap(),
    )
    .unwrap()
}

fn durable_host(runtime: RuntimeState) -> LiveHost {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    LiveHost::with_runtime_state(
        Limits::default(),
        boundary,
        Serving::new(ServingLimits::default()).unwrap(),
        runtime,
    )
    .unwrap()
}

fn durable_host_with_owner(runtime: RuntimeState, owner: [u8; 16]) -> LiveHost {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    LiveHost::with_runtime_state_and_owner(
        Limits::default(),
        boundary,
        Serving::new(ServingLimits::default()).unwrap(),
        runtime,
        owner,
    )
    .unwrap()
}

fn durable_host_after_takeover(
    runtime: RuntimeState,
    owner: [u8; 16],
    lost_owner: RequestOwner,
) -> LiveHost {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    LiveHost::with_runtime_state_after_takeover(
        Limits::default(),
        boundary,
        Serving::new(ServingLimits::default()).unwrap(),
        runtime,
        owner,
        lost_owner,
    )
    .unwrap()
}

fn durable_repository() -> (PathBuf, Repository) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("orna-live-v1-{nonce}"));
    fs::create_dir(&root).unwrap();
    let status = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(&root)
        .status()
        .unwrap();
    assert!(status.success());
    let repository = Repository::discover(&root).unwrap();
    (root, repository)
}

fn open_durable_state(repository: &Repository) -> RuntimeState {
    block_on(RuntimeState::open(
        repository,
        RuntimeIdentity {
            database_id: [6; 16],
            repository_id: [7; 16],
        },
        [8; 32],
    ))
    .unwrap()
}

fn request_fingerprint(bytes: &[u8], session: [u8; 16]) -> [u8; 32] {
    let envelope = Envelope::decode(bytes, Limits::default().protocol).unwrap();
    canonical_request_fingerprint(session, &envelope, Limits::default().protocol).unwrap()
}

fn remove_test_repository(root: &Path) {
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn http_decoder_handles_partial_body_and_pipelined_bytes() {
    let limits = TransportLimits::default();
    let request = b"POST /orna/session HTTP/1.1\r\nHost: example\r\nContent-Length: 4\r\n\r\nbodyGET /orna/session HTTP/1.1\r\n\r\n";
    assert_eq!(parse_http_request(&request[..68], limits).unwrap(), None);
    let parsed = parse_http_request(request, limits).unwrap().unwrap();
    assert_eq!(parsed.request().method, "POST");
    assert_eq!(parsed.request().path, "/orna/session");
    assert_eq!(parsed.request().body, b"body");
    assert_eq!(
        &request[parsed.consumed()..],
        b"GET /orna/session HTTP/1.1\r\n\r\n"
    );
}

#[test]
fn http_decoder_rejects_ambiguous_or_unsupported_framing() {
    let limits = TransportLimits::default();
    for raw in [
        b"POST /orna/session HTTP/1.1\r\nHost: example\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx"
            .as_slice(),
        b"POST /orna/session HTTP/1.1\r\nHost: example\r\nTransfer-Encoding: chunked\r\n\r\n"
            .as_slice(),
        b"GET /orna/session HTTP/1.0\r\nHost: example\r\n\r\n".as_slice(),
    ] {
        assert_eq!(
            parse_http_request(raw, limits),
            Err(HttpParseError::Malformed)
        );
    }
}

#[test]
fn http_decoder_applies_header_and_request_limits_before_body_materialisation() {
    let limits = TransportLimits {
        max_header_bytes: 32,
        ..TransportLimits::default()
    };
    assert_eq!(
        parse_http_request(
            b"GET /orna/session HTTP/1.1\r\nHost: example\r\n\r\n",
            limits
        ),
        Err(HttpParseError::Limit)
    );

    let limits = TransportLimits {
        max_request_bytes: 64,
        ..TransportLimits::default()
    };
    assert_eq!(
        parse_http_request(
            b"POST /orna/session HTTP/1.1\r\nHost: example\r\nContent-Length: 100\r\n\r\n",
            limits
        ),
        Err(HttpParseError::Limit)
    );
}

#[test]
fn http_response_encoder_serializes_bounded_responses_and_owns_content_length() {
    let response = WireResponse {
        status: 201,
        headers: vec![("content-type".into(), "application/json".into())],
        body: br#"{"ok":true}"#.to_vec(),
    };
    let encoded = response.encode_http(TransportLimits::default()).unwrap();
    assert_eq!(
        encoded,
        b"HTTP/1.1 201 Created\r\ncontent-type: application/json\r\nContent-Length: 11\r\n\r\n{\"ok\":true}"
    );

    let limited = TransportLimits {
        max_outgoing_bytes: encoded.len() - 1,
        ..TransportLimits::default()
    };
    assert_eq!(response.encode_http(limited), Err(HttpEncodeError::Limit));

    for response in [
        WireResponse {
            status: 200,
            headers: vec![("Content-Length".into(), "1".into())],
            body: vec![b'x'],
        },
        WireResponse {
            status: 200,
            headers: vec![("x-test".into(), "ok\r\nInjected: yes".into())],
            body: Vec::new(),
        },
        WireResponse {
            status: 204,
            headers: Vec::new(),
            body: vec![b'x'],
        },
    ] {
        assert_eq!(
            response.encode_http(TransportLimits::default()),
            Err(HttpEncodeError::Malformed)
        );
    }
}

#[test]
fn http_connection_retains_partial_reads_and_drains_pipelined_requests() {
    let mut connection = HttpConnection::new(TransportLimits::default());
    let request = b"GET /orna/session HTTP/1.1\r\nHost: example\r\n\r\nGET /orna/session HTTP/1.1\r\nHost: example\r\n\r\n";
    assert!(connection.push(&request[..20]).unwrap().is_empty());
    assert_eq!(connection.buffered_bytes(), 20);
    let requests = connection.push(&request[20..]).unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].request().path, "/orna/session");
    assert_eq!(requests[1].request().method, "GET");
    assert_eq!(connection.buffered_bytes(), 0);
}

#[test]
fn http_connection_bounds_an_incomplete_request_before_append() {
    let limits = TransportLimits {
        max_request_bytes: 32,
        ..TransportLimits::default()
    };
    let mut connection = HttpConnection::new(limits);
    assert_eq!(
        connection.push(b"GET /orna/session HTTP/1.1\r\nHost: "),
        Err(HttpParseError::Limit)
    );
    assert_eq!(connection.buffered_bytes(), 0);
}

#[test]
fn http_connection_driver_routes_partial_reads_and_encodes_the_response() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let body = format!(
        r#"{{"database":"{}","protocol":"{}"}}"#,
        uuid(2),
        SUBPROTOCOL
    );
    let request = format!(
        "POST /orna/session HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let request_bytes = request.as_bytes();
    let split = request.len() / 2;
    let mut authority = Authority;
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    assert!(
        block_on(transport.handle_http_read(
            &mut connection,
            &request_bytes[..split],
            0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        ))
        .unwrap()
        .is_empty()
    );
    let responses = block_on(transport.handle_http_read(
        &mut connection,
        &request_bytes[split..],
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ))
    .unwrap();
    assert_eq!(responses.len(), 1);
    assert!(responses[0].starts_with(b"HTTP/1.1 201 Created\r\n"));
    assert!(
        responses[0]
            .windows(b"Content-Length: ".len())
            .any(|window| window == b"Content-Length: ")
    );
    assert_eq!(connection.buffered_bytes(), 0);
    let mut rejected_connection = HttpConnection::new(TransportLimits::default());
    assert!(matches!(
        block_on(transport.handle_http_read(
            &mut rejected_connection,
            b"GET /orna/session HTTP/1.1\r\n\r\n",
            0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        )),
        Err(HttpConnectionError::Parse(HttpParseError::Malformed))
    ));
}

#[test]
fn async_http_connection_loop_writes_routed_responses_until_eof() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let body = format!(
        r#"{{"database":"{}","protocol":"{}"}}"#,
        uuid(2),
        SUBPROTOCOL
    );
    let request = format!(
        "POST /orna/session HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let mut reader = Cursor::new(request.into_bytes());
    let mut writer = Cursor::new(Vec::new());
    let mut authority = Authority;
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    block_on(transport.serve_http_connection(
        &mut reader,
        &mut writer,
        &mut connection,
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ))
    .unwrap();
    assert!(writer.into_inner().starts_with(b"HTTP/1.1 201 Created\r\n"));
    assert_eq!(connection.buffered_bytes(), 0);
}

#[test]
fn accepted_tcp_socket_routes_a_session_request_end_to_end() {
    let listener = LiveTransport::bind_default_listener(0).unwrap();
    let status = listener.status();
    assert_eq!(status.exposure, ListenerExposure::Loopback);
    assert!(status.address.ip().is_loopback());
    let address = status.address;
    let server = thread::spawn(move || {
        let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut authority = Authority;
        let mut issuer = Issuer(7, None);
        let mut deletion = Delete(true);
        transport.serve_one_http_listener(
            listener.listener(),
            &mut connection,
            &mut || 0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        )
    });

    let body = format!(
        r#"{{"database":"{}","protocol":"{}"}}"#,
        uuid(2),
        SUBPROTOCOL
    );
    let request = format!(
        "POST /orna/session HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let mut client = TcpStream::connect(address).unwrap();
    client.write_all(request.as_bytes()).unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).unwrap();
    server.join().unwrap().unwrap();

    assert!(response.starts_with(b"HTTP/1.1 201 Created\r\n"));
    assert!(response.windows(4).any(|window| window == b"\r\n\r\n"));
}

#[test]
fn listener_policy_reports_loopback_rejects_exposure_and_releases_on_drop() {
    let listener = LiveTransport::bind_default_listener(0).unwrap();
    let address = listener.status().address;
    assert_eq!(listener.status().exposure, ListenerExposure::Loopback);
    assert!(address.ip().is_loopback());

    assert!(matches!(
        LiveTransport::bind_explicit_listener(([0, 0, 0, 0], address.port()).into()),
        Err(ListenerBindError::NonLoopback)
    ));

    drop(listener);
    let _released = TcpListener::bind(address).unwrap();

    let listener = LiveTransport::bind_explicit_listener(([127, 0, 0, 1], 0).into()).unwrap();
    assert_eq!(listener.status().exposure, ListenerExposure::Explicit);
    assert!(listener.status().address.ip().is_loopback());
}

#[test]
fn cancellable_listener_accept_preserves_listener_ownership() {
    let listener = LiveTransport::bind_default_listener(0).unwrap();
    let address = listener.status().address;
    let transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut acceptor = PendingListenerAcceptor::default();
    let mut cancellation = CancelAfterPolls {
        polls: 0,
        ready_after: 2,
    };

    assert!(matches!(
        block_on(transport.accept_listener_with_cancellation(
            &listener,
            &mut acceptor,
            &mut cancellation,
        )),
        Err(HttpIoError::Cancelled)
    ));
    assert_eq!(acceptor.accepts, 1);
    assert_eq!(listener.listener().local_addr().unwrap(), address);
    assert!(TcpListener::bind(address).is_err());
}

#[test]
fn accepted_tcp_socket_hands_off_an_upgrade_to_the_websocket_driver() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
        let mut authority = Authority;
        let mut issuer = Issuer(7, None);
        let mut deletion = Delete(true);
        let created = block_on(transport.handle(
            wire(
                "POST",
                "/orna/session",
                &format!(
                    r#"{{"database":"{}","protocol":"{}"}}"#,
                    uuid(2),
                    SUBPROTOCOL
                ),
            ),
            0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        ));
        sender.send(token(&created)).unwrap();
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut application = UnitApplication::default();
        transport.serve_one_websocket_listener(
            &listener,
            &mut connection,
            [5; 16],
            &mut || 1,
            &mut application,
        )
    });

    let request = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        receiver.recv().unwrap()
    );
    let mut client = TcpStream::connect(address).unwrap();
    client.write_all(request.as_bytes()).unwrap();
    client.write_all(&masked(true, 9, b"hi")).unwrap();
    client.write_all(&masked(true, 8, b"")).unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).unwrap();
    server.join().unwrap().unwrap();

    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("serialized handshake response");
    assert!(response.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
    assert_eq!(&response[header_end + 4..], b"\x8a\x02hi\x88\x00");
}

#[test]
fn accepted_tcp_socket_rejects_an_unmasked_client_frame_with_protocol_close() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let (outcome_sender, outcome_receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
        let mut authority = Authority;
        let mut issuer = Issuer(7, None);
        let mut deletion = Delete(true);
        let created = block_on(transport.handle(
            wire(
                "POST",
                "/orna/session",
                &format!(
                    r#"{{"database":"{}","protocol":"{}"}}"#,
                    uuid(2),
                    SUBPROTOCOL
                ),
            ),
            0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        ));
        sender.send(token(&created)).unwrap();
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut application = UnitApplication::default();
        let result = transport.serve_one_websocket_listener(
            &listener,
            &mut connection,
            [5; 16],
            &mut || 1,
            &mut application,
        );
        outcome_sender.send((result, application.calls)).unwrap();
    });

    let request = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        receiver.recv().unwrap()
    );
    let mut client = TcpStream::connect(address).unwrap();
    client.write_all(request.as_bytes()).unwrap();
    client.write_all(&unmasked(true, 2, b"")).unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).unwrap();

    let header_end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("serialized handshake response");
    assert!(response.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
    assert_eq!(&response[header_end + 4..], b"\x88\x02\x03\xea");
    let (result, application_calls) = outcome_receiver.recv().unwrap();
    assert_eq!(result, Ok(()));
    assert_eq!(application_calls, 0);
    server.join().unwrap();
}

#[test]
fn accepted_websocket_eof_disconnects_its_attachment() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = listener.local_addr().unwrap();
    let (sender, receiver) = mpsc::channel();
    let server = thread::spawn(move || {
        let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
        let mut authority = Authority;
        let mut issuer = Issuer(7, None);
        let mut deletion = Delete(true);
        let created = block_on(transport.handle(
            wire(
                "POST",
                "/orna/session",
                &format!(
                    r#"{{"database":"{}","protocol":"{}"}}"#,
                    uuid(2),
                    SUBPROTOCOL
                ),
            ),
            0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        ));
        sender.send(token(&created)).unwrap();
        let mut connection = HttpConnection::new(TransportLimits::default());
        let mut application = UnitApplication::default();
        transport
            .serve_one_websocket_listener(
                &listener,
                &mut connection,
                [5; 16],
                &mut || 1,
                &mut application,
            )
            .unwrap();
        block_on(transport.receive(
            &mut WebSocketState::new([5; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        ))
    });

    let request = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        receiver.recv().unwrap()
    );
    let mut client = TcpStream::connect(address).unwrap();
    client.write_all(request.as_bytes()).unwrap();
    client.shutdown(Shutdown::Write).unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).unwrap();

    assert!(response.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
    assert_eq!(server.join().unwrap(), Err(Error::Closed));
}

#[test]
fn async_http_connection_loop_rejects_truncated_eof() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut reader = Cursor::new(b"GET /orna/session HTTP/1.1\r\nHost: example\r\n".to_vec());
    let mut writer = Cursor::new(Vec::new());
    let mut authority = Authority;
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    assert_eq!(
        block_on(transport.serve_http_connection(
            &mut reader,
            &mut writer,
            &mut connection,
            0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        )),
        Err(HttpIoError::Transport(HttpConnectionError::Parse(
            HttpParseError::Incomplete
        )))
    );
    assert!(writer.into_inner().is_empty());
}

#[test]
fn async_http_connection_loop_writes_before_admitting_next_pipelined_request() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let body = format!(
        r#"{{"database":"{}","protocol":"{}"}}"#,
        uuid(2),
        SUBPROTOCOL
    );
    let request = format!(
        "POST /orna/session HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let mut reader = Cursor::new([request.as_bytes(), request.as_bytes()].concat());
    let mut writer = FailFirstWriter { writes: 0 };
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    assert_eq!(
        block_on(transport.serve_http_connection(
            &mut reader,
            &mut writer,
            &mut connection,
            0,
            &mut authority,
            &mut issuer,
            &mut deletion,
        )),
        Err(HttpIoError::Write)
    );
    assert_eq!(authority.calls, 1);
}

#[test]
fn create_rejects_a_retained_session_identity_without_rotating_its_credential() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut authority = Authority;
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    let body = format!(
        r#"{{"database":"{}","protocol":"{}"}}"#,
        uuid(2),
        SUBPROTOCOL
    );
    let first = block_on(transport.handle(
        wire("POST", "/orna/session", &body),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let credential = token(&first);
    let second = block_on(transport.handle(
        wire("POST", "/orna/session", &body),
        1,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));

    assert_eq!(second.status, 503);
    assert_eq!(
        String::from_utf8(second.body).unwrap(),
        "{\"code\":\"live.unavailable\",\"message\":\"request rejected\"}"
    );
    assert_eq!(
        issuer.0, 8,
        "collision must not issue a replacement credential"
    );
    assert_eq!(
        block_on(transport.upgrade(websocket_upgrade(1, &credential), [4; 16], 1)).status,
        101,
        "the original session credential must remain usable"
    );
}

#[test]
fn async_http_connection_loop_samples_clock_for_each_request() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let body = format!(
        r#"{{"database":"{}","protocol":"{}"}}"#,
        uuid(2),
        SUBPROTOCOL
    );
    let request = format!(
        "POST /orna/session HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let mut reader = Cursor::new([request.as_bytes(), request.as_bytes()].concat());
    let mut writer = Cursor::new(Vec::new());
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    let mut times = vec![17, 23];
    block_on(transport.serve_http_connection_with_clock(
        &mut reader,
        &mut writer,
        &mut connection,
        &mut || times.remove(0),
        &mut authority,
        &mut issuer,
        &mut deletion,
    ))
    .unwrap();
    assert_eq!(authority.times, [17, 23]);
}

#[test]
fn async_http_connection_loop_cancels_a_pending_read() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut reader = PendingReader;
    let mut writer = Cursor::new(Vec::new());
    let mut authority = Authority;
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    let mut clock = || 0;
    let mut cancellation = CancelAfterPolls {
        polls: 0,
        ready_after: 2,
    };
    assert_eq!(
        block_on(transport.serve_http_connection_with_cancellation(
            &mut reader,
            &mut writer,
            &mut connection,
            &mut clock,
            &mut cancellation,
            &mut authority,
            &mut issuer,
            &mut deletion,
        )),
        Err(HttpIoError::Cancelled)
    );
    assert!(writer.into_inner().is_empty());
}

#[test]
fn async_http_connection_loop_cancels_a_pending_write() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let body = format!(
        r#"{{"database":"{}","protocol":"{}"}}"#,
        uuid(2),
        SUBPROTOCOL
    );
    let request = format!(
        "POST /orna/session HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let mut reader = Cursor::new(request.into_bytes());
    let mut writer = PendingWriter;
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut issuer = Issuer(7, None);
    let mut deletion = Delete(true);
    let mut clock = || 0;
    let mut cancellation = CancelAfterPolls {
        polls: 0,
        ready_after: 3,
    };
    assert_eq!(
        block_on(transport.serve_http_connection_with_cancellation(
            &mut reader,
            &mut writer,
            &mut connection,
            &mut clock,
            &mut cancellation,
            &mut authority,
            &mut issuer,
            &mut deletion,
        )),
        Err(HttpIoError::Cancelled)
    );
    assert_eq!(authority.calls, 1);
}

#[test]
fn websocket_connection_driver_preserves_a_co_read_frame_after_upgrade() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let cookie = token(&created);
    let upgrade = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        cookie
    );
    let mut input = upgrade.into_bytes();
    input.extend(masked(true, 9, b"hi"));
    let mut reader = Cursor::new(input);
    let mut writer = Cursor::new(Vec::new());
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = std::future::pending::<()>();
    block_on(transport.serve_websocket_connection(
        &mut reader,
        &mut writer,
        &mut connection,
        [5; 16],
        &mut clock,
        &mut cancellation,
        &mut application,
    ))
    .unwrap();
    let output = writer.into_inner();
    let header_end = output
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("serialized handshake response");
    assert!(output.starts_with(b"HTTP/1.1 101 Switching Protocols\r\n"));
    assert!(
        !output[..header_end]
            .windows(16)
            .any(|window| window == b"Content-Length: ")
    );
    assert_eq!(&output[header_end + 4..], b"\x8a\x02hi");
}

#[test]
fn websocket_connection_driver_emits_exact_canonical_result_envelope() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let input = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        token(&created)
    );
    let mut bytes = input.into_bytes();
    bytes.extend(masked(true, 2, &eval([1; 16], [12; 16], "1")));
    let mut reader = Cursor::new(bytes);
    let mut writer = RecordingWriter::default();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = std::future::pending::<()>();
    block_on(transport.serve_websocket_connection(
        &mut reader,
        &mut writer,
        &mut connection,
        [5; 16],
        &mut clock,
        &mut cancellation,
        &mut application,
    ))
    .unwrap();
    assert_eq!(application.calls, 1);
    let output = writer.bytes;
    let header_end = output
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("serialized handshake response");
    assert_eq!(
        &output[header_end + 4..],
        [
            0x82, 0x47, 0xa5, 0x00, 0x01, 0x01, 0x12, 0x02, 0x50, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c,
            0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x0c, 0x03, 0xf6, 0x04,
            0xa4, 0x00, 0x00, 0x01, 0xd9, 0xea, 0x6e, 0x80, 0x02, 0x58, 0x20, 0x1b, 0xb0, 0xd4,
            0x0c, 0x76, 0x63, 0x36, 0x3d, 0xb5, 0x3a, 0x62, 0xce, 0xc1, 0x03, 0x2c, 0x9f, 0x1d,
            0x86, 0xc4, 0x26, 0x70, 0x08, 0xc1, 0x3b, 0x8c, 0xb6, 0x46, 0x0d, 0x0e, 0xde, 0x23,
            0x35, 0x03, 0xf6,
        ]
    );
}

#[test]
fn websocket_connection_driver_cancellation_after_delivery_commits_then_cleans_attachment() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let credential = token(&created);
    assert_eq!(
        block_on(transport.upgrade(websocket_upgrade(1, &credential), [4; 16], 1)).status,
        101
    );
    let input = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        credential
    );
    let mut reader = PrefixThenPendingReader {
        prefix: Cursor::new(input.into_bytes()),
    };
    let mut writer = Cursor::new(Vec::new());
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = CancelAfterPolls {
        polls: 0,
        ready_after: 4,
    };
    assert_eq!(
        block_on(transport.serve_websocket_connection(
            &mut reader,
            &mut writer,
            &mut connection,
            [5; 16],
            &mut clock,
            &mut cancellation,
            &mut application,
        )),
        Err(HttpIoError::Cancelled)
    );
    assert_eq!(
        block_on(transport.receive(
            &mut WebSocketState::new([5; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        )),
        Err(Error::Closed)
    );
    assert_eq!(
        block_on(transport.receive(
            &mut WebSocketState::new([4; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        )),
        Err(Error::Closed)
    );
    assert!(
        writer
            .into_inner()
            .starts_with(b"HTTP/1.1 101 Switching Protocols\r\n")
    );
    assert_eq!(transport.take_retired_attachments(), vec![[4; 16]]);
}

#[test]
fn websocket_connection_driver_cancellation_during_output_disconnects_its_attachment() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let input = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        token(&created)
    );
    let mut bytes = input.into_bytes();
    bytes.extend(masked(true, 9, b"p"));
    let mut reader = Cursor::new(bytes);
    let mut writer = Cursor::new(Vec::new());
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = CancelAfterPolls {
        polls: 0,
        ready_after: 4,
    };
    assert_eq!(
        block_on(transport.serve_websocket_connection(
            &mut reader,
            &mut writer,
            &mut connection,
            [6; 16],
            &mut clock,
            &mut cancellation,
            &mut application,
        )),
        Err(HttpIoError::Cancelled)
    );
    assert_eq!(
        block_on(transport.receive(
            &mut WebSocketState::new([6; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        )),
        Err(Error::Closed)
    );
}

#[test]
fn websocket_connection_driver_output_write_failure_disconnects_its_attachment() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let mut input = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        token(&created)
    )
    .into_bytes();
    input.extend(masked(true, 9, b"p"));
    let mut reader = Cursor::new(input);
    let mut writer = FailAfterFirstWrite { writes: 0 };
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = std::future::pending::<()>();
    assert_eq!(
        block_on(transport.serve_websocket_connection(
            &mut reader,
            &mut writer,
            &mut connection,
            [7; 16],
            &mut clock,
            &mut cancellation,
            &mut application,
        )),
        Err(HttpIoError::Write)
    );
    assert_eq!(
        block_on(transport.receive(
            &mut WebSocketState::new([7; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        )),
        Err(Error::Closed)
    );
}

#[test]
fn websocket_connection_driver_delivers_co_read_frames_before_admitting_the_next() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let input = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        token(&created)
    );
    let mut bytes = input.into_bytes();
    bytes.extend(masked(true, 9, b"one"));
    bytes.extend(masked(true, 9, b"two"));
    let mut reader = Cursor::new(bytes);
    let mut writer = RecordingWriter::default();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = std::future::pending::<()>();
    block_on(transport.serve_websocket_connection(
        &mut reader,
        &mut writer,
        &mut connection,
        [5; 16],
        &mut clock,
        &mut cancellation,
        &mut application,
    ))
    .unwrap();
    assert_eq!(writer.flushes, 3);
    let header_end = writer
        .bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    assert_eq!(&writer.bytes[header_end + 4..], b"\x8a\x03one\x8a\x03two");
}

#[test]
fn websocket_connection_driver_does_not_commit_an_upgrade_before_handshake_delivery() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let input = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        token(&created)
    );
    let request = parse_http_request(input.as_bytes(), TransportLimits::default())
        .unwrap()
        .unwrap()
        .request()
        .clone();
    assert_eq!(
        block_on(transport.upgrade(request.clone(), [4; 16], 1)).status,
        101
    );
    let mut reader = Cursor::new(input.into_bytes());
    let mut writer = FailFirstWriter { writes: 0 };
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = std::future::pending::<()>();
    assert_eq!(
        block_on(transport.serve_websocket_connection(
            &mut reader,
            &mut writer,
            &mut connection,
            [5; 16],
            &mut clock,
            &mut cancellation,
            &mut application,
        )),
        Err(HttpIoError::Write)
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([4; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
    // The failed 101 write aborts only the candidate handoff. The active
    // attachment remains usable above; the candidate stays fenced until the
    // executable owner cancels and joins it.
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
    assert_eq!(
        transport
            .begin_websocket_upgrade(&request, [5; 16], 2)
            .unwrap_err()
            .status,
        503
    );
    assert!(transport.acknowledge_retired_attachment([5; 16]));
    assert_eq!(block_on(transport.upgrade(request, [5; 16], 2)).status, 101);
    assert_eq!(transport.take_retired_attachments(), vec![[4; 16]]);
}

#[test]
fn websocket_connection_driver_cancellation_retires_the_stalled_candidate_before_failure() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let credential = token(&created);
    let request = websocket_upgrade(1, &credential);
    assert_eq!(
        block_on(transport.upgrade(request.clone(), [4; 16], 1)).status,
        101
    );

    let mut reader = Cursor::new(
        format!(
            "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
            uuid(1), SUBPROTOCOL, credential
        )
        .into_bytes(),
    );
    let mut writer = PendingWriter;
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 2;
    let mut cancellation = CancelAfterPolls {
        polls: 0,
        ready_after: 3,
    };

    assert_eq!(
        block_on(transport.serve_websocket_connection(
            &mut reader,
            &mut writer,
            &mut connection,
            [5; 16],
            &mut clock,
            &mut cancellation,
            &mut application,
        )),
        Err(HttpIoError::Cancelled)
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);

    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([4; 16]),
            2,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
    assert_eq!(block_on(transport.upgrade(request, [6; 16], 2)).status, 101);
    assert_eq!(transport.take_retired_attachments(), vec![[4; 16]]);
}

#[test]
fn websocket_connection_driver_closes_after_a_peer_close() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let input = format!(
        "GET /orna/live/{} HTTP/1.1\r\nHost: app.example\r\nOrigin: https://app.example\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Protocol: {}\r\nCookie: orna_session={}\r\n\r\n",
        uuid(1),
        SUBPROTOCOL,
        token(&created)
    );
    let mut bytes = input.into_bytes();
    bytes.extend(masked(true, 8, b""));
    let mut reader = Cursor::new(bytes);
    let mut writer = RecordingWriter::default();
    let mut connection = HttpConnection::new(TransportLimits::default());
    let mut application = UnitApplication::default();
    let mut clock = || 1;
    let mut cancellation = std::future::pending::<()>();
    block_on(transport.serve_websocket_connection(
        &mut reader,
        &mut writer,
        &mut connection,
        [5; 16],
        &mut clock,
        &mut cancellation,
        &mut application,
    ))
    .unwrap();
    assert_eq!(writer.closes, 1);
}
fn subscribe() -> Vec<u8> {
    subscribe_request([3; 16])
}

fn subscribe_request(request: [u8; 16]) -> Vec<u8> {
    Envelope {
        request: Some(request),
        watch: None,
        message: Message::Subscribe {
            resource: [4; 16],
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "dark".into(),
                supported_kinds: vec![],
            },
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap()
}
fn cancel() -> Vec<u8> {
    cancel_request([7; 16], [8; 16])
}
fn cancel_request(request: [u8; 16], target: [u8; 16]) -> Vec<u8> {
    Envelope {
        request: Some(request),
        watch: None,
        message: Message::Cancel {
            target_kind: TargetKind::Request,
            target,
        },
        extensions: BTreeMap::new(),
    }
.encode(Limits::default().protocol)
.unwrap()
}
fn resync() -> Vec<u8> {
    resync_request([9; 16], [10; 16])
}

fn resync_request(request: [u8; 16], watch: [u8; 16]) -> Vec<u8> {
    Envelope {
        request: Some(request),
        watch: Some(watch),
        message: Message::Resync,
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap()
}

fn event(session: [u8; 16], request: [u8; 16], watch: [u8; 16]) -> Vec<u8> {
    let mut envelope = Envelope {
        request: Some(request),
        watch: Some(watch),
        message: Message::Event {
            revision: 0,
            action: [13; 16],
            value: CanonicalValue::unit(),
            fingerprint: [0; 32],
        },
        extensions: BTreeMap::new(),
    };
    let fingerprint =
        canonical_request_fingerprint(session, &envelope, Limits::default().protocol).unwrap();
    if let Message::Event {
        fingerprint: sent, ..
    } = &mut envelope.message
    {
        *sent = fingerprint;
    }
    envelope.encode(Limits::default().protocol).unwrap()
}

fn unsubscribe() -> Vec<u8> {
    Envelope {
        request: Some([12; 16]),
        watch: Some([11; 16]),
        message: Message::Unsubscribe,
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap()
}

fn eval(session: [u8; 16], request: [u8; 16], source: &str) -> Vec<u8> {
    let mut envelope = Envelope {
        request: Some(request),
        watch: None,
        message: Message::Eval {
            source: source.into(),
            database: DatabaseContext {
                database: [2; 16],
                snapshot: None,
            },
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "terminal/dark".into(),
                supported_kinds: vec![],
            },
            fingerprint: [0; 32],
        },
        extensions: BTreeMap::new(),
    };
    let fingerprint =
        canonical_request_fingerprint(session, &envelope, Limits::default().protocol).unwrap();
    if let Message::Eval {
        fingerprint: sent, ..
    } = &mut envelope.message
    {
        *sent = fingerprint;
    }
    envelope.encode(Limits::default().protocol).unwrap()
}

fn eval_with_context(
    session: [u8; 16],
    request: [u8; 16],
    database: [u8; 16],
    snapshot: Option<CanonicalSnapshot>,
) -> Vec<u8> {
    let mut envelope = Envelope {
        request: Some(request),
        watch: None,
        message: Message::Eval {
            source: LIVE_RUNTIME_BOUNDARY_FIXTURE.into(),
            database: DatabaseContext { database, snapshot },
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "terminal/dark".into(),
                supported_kinds: vec![],
            },
            fingerprint: [0; 32],
        },
        extensions: BTreeMap::new(),
    };
    let fingerprint =
        canonical_request_fingerprint(session, &envelope, Limits::default().protocol).unwrap();
    if let Message::Eval {
        fingerprint: sent, ..
    } = &mut envelope.message
    {
        *sent = fingerprint;
    }
    envelope.encode(Limits::default().protocol).unwrap()
}

fn watch_with_context(request: [u8; 16], database: [u8; 16]) -> Vec<u8> {
    Envelope {
        request: Some(request),
        watch: None,
        message: Message::Watch {
            source: LIVE_RUNTIME_BOUNDARY_FIXTURE.into(),
            database: DatabaseContext {
                database,
                snapshot: None,
            },
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: None,
                theme: "terminal/dark".into(),
                supported_kinds: vec![],
            },
            refresh_floor: None,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap()
}

fn create(host: &mut LiveHost, issuer: &mut Issuer) -> SessionCredential {
    block_on(host.create(
        CreateRequest {
            id: [1; 16],
            origin: origin(),
            expires_at: 100,
            now: 0,
            subscribe: &subscribe(),
        },
        issuer,
    ))
    .unwrap()
}

#[test]
fn http_create_and_resume_negotiate_and_replace_connections() {
    assert_eq!(
        LiveHost::negotiate_subprotocol(&["other", SUBPROTOCOL]),
        Ok(SUBPROTOCOL)
    );
    assert_eq!(
        LiveHost::negotiate_subprotocol(&["other"]),
        Err(Error::UnsupportedSubprotocol)
    );
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [5; 16],
            now: 1
        }))
        .unwrap(),
        orna_security_v1::AttachOutcome::Attached
    );
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [6; 16],
            now: 2
        }))
        .unwrap(),
        orna_security_v1::AttachOutcome::Replaced(AttachmentId::new([5; 16]))
    );
    assert_eq!(
        block_on(host.handle_frame([5; 16], 3, Frame::Close)),
        Err(Error::Closed)
    );
    assert_eq!(
        block_on(host.handle_frame([6; 16], 3, Frame::Close)),
        Ok(FrameOutcome::Closed)
    );
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [7; 16],
            now: 4,
        }))
        .unwrap(),
        orna_security_v1::AttachOutcome::Reconnected
    );
}

#[test]
fn rejected_cross_layer_reconnect_preserves_a_valid_session() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    let mismatched = SessionCredential {
        security: credential.security.clone(),
        serving: ServingCredential::new([9; 32]),
    };

    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &mismatched,
            attachment: [5; 16],
            now: 1,
        })),
        Err(Error::Denied)
    );
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [6; 16],
            now: 1,
        })),
        Ok(orna_security_v1::AttachOutcome::Attached)
    );
}

#[test]
fn http_resume_retires_the_active_attachment_without_closing_the_session() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [5; 16],
            now: 1,
        }))
        .unwrap(),
        orna_security_v1::AttachOutcome::Attached
    );

    let mut replacement_issuer = Issuer(2, None);
    let (replacement, retired) = block_on(host.rotate_and_retire(
        [1; 16],
        &origin(),
        &credential,
        2,
        &mut replacement_issuer,
    ))
    .unwrap();
    assert_eq!(retired, Some([5; 16]));
    assert_eq!(
        block_on(host.handle_frame([5; 16], 3, Frame::Close)),
        Err(Error::Closed)
    );
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &replacement,
            attachment: [6; 16],
            now: 3,
        }))
        .unwrap(),
        orna_security_v1::AttachOutcome::Reconnected
    );
    assert_eq!(
        block_on(host.handle_frame([6; 16], 4, Frame::Close)),
        Ok(FrameOutcome::Closed)
    );
}

#[test]
fn websocket_replacement_queues_retirement_and_close_is_idempotent() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let cookie = token(&created);
    let upgrade = |attachment| {
        let mut request = wire("GET", "/orna/live/01010101-0101-0101-0101-010101010101", "");
        request.headers.extend([
            ("connection".into(), "Upgrade".into()),
            ("upgrade".into(), "websocket".into()),
            ("sec-websocket-version".into(), "13".into()),
            (
                "sec-websocket-key".into(),
                "dGhlIHNhbXBsZSBub25jZQ==".into(),
            ),
            ("sec-websocket-protocol".into(), SUBPROTOCOL.into()),
            ("cookie".into(), format!("orna_session={cookie}")),
        ]);
        (request, attachment)
    };
    assert_eq!(
        block_on(transport.upgrade(upgrade([5; 16]).0, [5; 16], 1)).status,
        101
    );
    assert_eq!(
        block_on(transport.upgrade(upgrade([6; 16]).0, [6; 16], 2)).status,
        101
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
    assert_eq!(
        block_on(transport.upgrade(upgrade([5; 16]).0, [5; 16], 3)).status,
        503
    );
    assert_eq!(
        block_on(transport.close_attachment([5; 16], 3)),
        Err(Error::Closed)
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([6; 16]),
            3,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
    assert!(transport.acknowledge_retired_attachment([5; 16]));
    assert_eq!(
        block_on(transport.upgrade(upgrade([5; 16]).0, [5; 16], 4)).status,
        101
    );
    assert_eq!(transport.take_retired_attachments(), vec![[6; 16]]);
    assert_eq!(
        block_on(transport.close_attachment([6; 16], 4)),
        Err(Error::Closed)
    );
    assert_eq!(
        block_on(transport.close_attachment([5; 16], 4)),
        Ok(FrameOutcome::Closed)
    );
    assert!(transport.acknowledge_retired_attachment([6; 16]));
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
    assert_eq!(
        block_on(transport.upgrade(upgrade([5; 16]).0, [5; 16], 5)).status,
        503
    );
    assert_eq!(
        block_on(transport.close_attachment([5; 16], 5)),
        Err(Error::Closed)
    );
    assert!(transport.take_retired_attachments().is_empty());
    assert!(transport.acknowledge_retired_attachment([5; 16]));
    assert_eq!(
        block_on(transport.upgrade(upgrade([5; 16]).0, [5; 16], 6)).status,
        101
    );
}

#[test]
fn live_requests_cannot_switch_the_issued_database_or_cwd_runtime() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(created.status, 201);
    assert_eq!(
        block_on(transport.upgrade(websocket_upgrade(1, &token(&created)), [5; 16], 1)).status,
        101
    );

    let mut socket = WebSocketState::new([5; 16]);
    let other_database = eval_with_context([1; 16], [21; 16], [3; 16], None);
    let prepared = block_on(transport.prepare_websocket_application(
        &mut socket,
        2,
        &masked_binary_payload(&other_database),
    ))
    .expect("a decoded database rejection is returned as a wire diagnostic");
    let WebSocketApplicationPreparation::Output(WebSocketOutput::Binary { payload, .. }) = prepared
    else {
        panic!("database mismatch should produce a correlated diagnostic");
    };
    let rejected = Envelope::decode(&payload, Limits::default().protocol).unwrap();
    assert_eq!(rejected.request, Some([21; 16]));
    assert!(matches!(rejected.message, Message::Diagnostic { .. }));

    let mut application = UnitApplication::default();
    let rejected = block_on(transport.receive_with_application(
        &mut socket,
        2,
        &masked_binary_payload(&other_database),
        &mut application,
    ))
    .unwrap();
    assert_eq!(rejected.len(), 1);
    let WebSocketOutput::Binary { payload, .. } = &rejected[0] else {
        panic!("a rejected request should receive a correlated diagnostic");
    };
    let rejected = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert!(matches!(rejected.message, Message::Diagnostic { .. }));
    assert_eq!(application.calls, 0);

    let other_runtime_snapshot = CanonicalSnapshot::cwd([2; 16], [4; 16], 0.into()).unwrap();
    let other_runtime = eval_with_context(
        [1; 16],
        [22; 16],
        [2; 16],
        Some(other_runtime_snapshot),
    );
    let prepared = block_on(transport.prepare_websocket_application(
        &mut socket,
        2,
        &masked_binary_payload(&other_runtime),
    ))
    .expect("a decoded runtime rejection is returned as a wire diagnostic");
    let WebSocketApplicationPreparation::Output(WebSocketOutput::Binary { payload, .. }) = prepared
    else {
        panic!("runtime mismatch should produce a correlated diagnostic");
    };
    let rejected = Envelope::decode(&payload, Limits::default().protocol).unwrap();
    assert_eq!(rejected.request, Some([22; 16]));
    assert!(matches!(rejected.message, Message::Diagnostic { .. }));

    let other_database_watch = watch_with_context([23; 16], [3; 16]);
    let prepared = block_on(transport.prepare_websocket_application(
        &mut socket,
        2,
        &masked_binary_payload(&other_database_watch),
    ))
    .expect("a decoded watch rejection is returned as a wire diagnostic");
    let WebSocketApplicationPreparation::Output(WebSocketOutput::Binary { payload, .. }) = prepared
    else {
        panic!("watch mismatch should produce a correlated diagnostic");
    };
    let rejected = Envelope::decode(&payload, Limits::default().protocol).unwrap();
    assert_eq!(rejected.request, Some([23; 16]));
    assert!(matches!(rejected.message, Message::Diagnostic { .. }));

    let bound_request = eval_with_context([1; 16], [24; 16], [2; 16], None);
    let accepted = block_on(transport.receive_with_application(
        &mut socket,
        2,
        &masked_binary_payload(&bound_request),
        &mut application,
    ))
    .unwrap();
    assert_eq!(accepted.len(), 1);
    assert!(matches!(&accepted[0], WebSocketOutput::Binary { .. }));
    assert_eq!(application.calls, 1);

    let bound_snapshot = CanonicalSnapshot::cwd([2; 16], [3; 16], 0.into()).unwrap();
    let bound_snapshot_request = eval_with_context(
        [1; 16],
        [25; 16],
        [2; 16],
        Some(bound_snapshot),
    );
    let accepted = block_on(transport.receive_with_application(
        &mut socket,
        2,
        &masked_binary_payload(&bound_snapshot_request),
        &mut application,
    ))
    .unwrap();
    assert_eq!(accepted.len(), 1);
    assert!(matches!(&accepted[0], WebSocketOutput::Binary { .. }));
    assert_eq!(application.calls, 2);
}

#[test]
fn async_rejection_after_session_delete_stays_behind_the_session_fence() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(r#"{{"database":"{}","protocol":"{}"}}"#, uuid(2), SUBPROTOCOL),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(created.status, 201);
    let session_token = token(&created);
    assert_eq!(
        block_on(transport.upgrade(
            websocket_upgrade(1, &session_token),
            [5; 16],
            1,
        ))
        .status,
        101
    );

    let mut socket = WebSocketState::new([5; 16]);
    let preparation = block_on(transport.prepare_websocket_application(
        &mut socket,
        2,
        &masked_binary_payload(&eval_with_context([1; 16], [30; 16], [2; 16], None)),
    ))
    .unwrap();
    let WebSocketApplicationPreparation::Work(ticket) = preparation else {
        panic!("the fixture-backed evaluation should be admitted as async work");
    };
    let completion = ticket.reject(Error::UnsupportedOperation);

    assert_eq!(
        block_on(transport.close_attachment([5; 16], 2)),
        Ok(FrameOutcome::Closed)
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
    assert!(transport.acknowledge_retired_attachment([5; 16]));

    let mut delete = wire("DELETE", "/orna/session/01010101-0101-0101-0101-010101010101", "");
    delete.headers.push((
        "authorization".into(),
        format!("Bearer {session_token}"),
    ));
    let mut children = RecordingChildren::default();
    let deleted = block_on(transport.handle_with_children(
        delete,
        3,
        &mut authority,
        &mut issuer,
        &mut deletion,
        &mut children,
    ));
    assert_eq!(deleted.status, 204);
    assert_eq!(children.requests, vec![RequestIdentity {
        session_id: [1; 16],
        request_id: [30; 16],
    }]);

    assert_eq!(
        block_on(transport.complete_application(completion)),
        Err(Error::Closed),
        "a stale rejection must not become a diagnostic after the session was deleted"
    );
}

#[test]
fn async_rejection_keeps_watch_correlation_after_session_resume() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(r#"{{"database":"{}","protocol":"{}"}}"#, uuid(2), SUBPROTOCOL),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(created.status, 201);
    let session_token = token(&created);
    assert_eq!(
        block_on(transport.upgrade(
            websocket_upgrade(1, &session_token),
            [5; 16],
            1,
        ))
        .status,
        101
    );

    let mut socket = WebSocketState::new([5; 16]);
    let mut watch_application = IdentityWatchApplication {
        next_revision: 0,
        next_present: IdentityWatchApplication::present("after-resume"),
    };
    let subscribed = block_on(transport.receive_with_application(
        &mut socket,
        2,
        &masked_binary_payload(&subscribe_request([40; 16])),
        &mut watch_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &subscribed[0] else {
        panic!("subscribe should return its initial snapshot");
    };
    let snapshot = Envelope::decode(payload, Limits::default().protocol).unwrap();
    let watch = snapshot.watch.expect("the session owns a live watch");
    assert_eq!(watch, [55; 16]);

    // Exercise the crate-local Orna source fixture in the same session before
    // checking that a delayed event rejection retains its watch identity.
    let mut eval_application = UnitApplication::default();
    let fixture_eval = eval_with_context([1; 16], [42; 16], [2; 16], None);
    assert_eq!(
        block_on(transport.receive_with_application(
            &mut socket,
            2,
            &masked_binary_payload(&fixture_eval),
            &mut eval_application,
        ))
        .unwrap()
        .len(),
        1
    );

    let preparation = block_on(transport.prepare_websocket_application(
        &mut socket,
        2,
        &masked_binary_payload(&event([1; 16], [41; 16], watch)),
    ))
    .unwrap();
    let WebSocketApplicationPreparation::Work(ticket) = preparation else {
        panic!("the watched event should be admitted as async work");
    };
    let completion = ticket.reject(Error::Denied);

    let resumed = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session/01010101-0101-0101-0101-010101010101/resume",
            &format!(
                r#"{{"resume_token":"{session_token}","protocol":"{SUBPROTOCOL}"}}"#
            ),
        ),
        3,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(resumed.status, 200);
    assert_ne!(token(&resumed), session_token);
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
    assert!(transport.acknowledge_retired_attachment([5; 16]));

    let WebSocketOutput::Binary { outcome, payload } =
        block_on(transport.complete_application(completion)).unwrap()
    else {
        panic!("the live event rejection should be correlated on the wire");
    };
    assert_eq!(outcome, FrameOutcome::Accepted);
    let rejected = Envelope::decode(&payload, Limits::default().protocol).unwrap();
    assert_eq!(rejected.request, Some([41; 16]));
    assert_eq!(rejected.watch, Some(watch));
    assert!(matches!(rejected.message, Message::Diagnostic { .. }));
}

#[test]
fn synchronous_event_diagnostic_replays_after_watch_closure() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = WatchEventApplication {
        mode: WatchEventMode::Denied,
        subscriptions: 0,
        events: 0,
    };

    let fixture_eval = block_on(host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(eval_with_context([1; 16], [36; 16], [2; 16], None)),
        &mut application,
    ))
    .unwrap();
    assert!(matches!(
        fixture_eval.response.unwrap().message,
        Message::Diagnostic { .. }
    ));

    let subscribed = block_on(host.dispatch_frame(
        [5; 16],
        3,
        Frame::Binary(subscribe_request([35; 16])),
        &mut application,
    ))
    .unwrap();
    let watch = subscribed
        .response
        .and_then(|response| response.watch)
        .expect("subscription returns its host watch identity");

    let request = event([1; 16], [37; 16], watch);
    let first = block_on(host.dispatch_frame(
        [5; 16],
        4,
        Frame::Binary(request.clone()),
        &mut application,
    ))
    .unwrap();
    let diagnostic = first.response.expect("portable rejection has a response");
    assert_eq!(diagnostic.request, Some([37; 16]));
    assert_eq!(diagnostic.watch, Some(watch));
    assert!(matches!(diagnostic.message, Message::Diagnostic { .. }));

    block_on(host.dispatch_frame(
        [5; 16],
        5,
        Frame::Binary(unsubscribe()),
        &mut application,
    ))
    .unwrap();
    let replay = block_on(host.dispatch_frame(
        [5; 16],
        6,
        Frame::Binary(request),
        &mut application,
    ))
    .unwrap();
    assert_eq!(replay.response, Some(diagnostic));
}

#[test]
fn durable_event_diagnostic_status_and_replay_keep_closed_watch_correlation() {
    let (root, repository) = durable_repository();
    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = WatchEventApplication {
        mode: WatchEventMode::Denied,
        subscriptions: 0,
        events: 0,
    };

    let fixture_eval = block_on(host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(eval_with_context([1; 16], [36; 16], [2; 16], None)),
        &mut application,
    ))
    .unwrap();
    assert!(matches!(
        fixture_eval.response.unwrap().message,
        Message::Diagnostic { .. }
    ));

    let subscribed = block_on(host.dispatch_frame(
        [5; 16],
        3,
        Frame::Binary(subscribe_request([35; 16])),
        &mut application,
    ))
    .unwrap();
    let watch = subscribed
        .response
        .and_then(|response| response.watch)
        .expect("subscription returns its host watch identity");
    let request = event([1; 16], [37; 16], watch);
    let request_fingerprint = request_fingerprint(&request, [1; 16]);
    let first = block_on(host.dispatch_frame(
        [5; 16],
        4,
        Frame::Binary(request.clone()),
        &mut application,
    ))
    .unwrap();
    let diagnostic = first.response.expect("portable rejection has a response");
    assert_eq!(diagnostic.request, Some([37; 16]));
    assert_eq!(diagnostic.watch, Some(watch));
    assert!(matches!(diagnostic.message, Message::Diagnostic { .. }));
    let stored = block_on(open_durable_state(&repository).request_status_for_identity(
        RequestIdentity {
            session_id: [1; 16],
            request_id: [37; 16],
        },
    ))
    .unwrap()
    .unwrap();
    assert_eq!(stored.state, RequestState::Completed);
    assert_eq!(
        Envelope::decode(
            stored.terminal_outcome.as_ref().unwrap().as_bytes(),
            Limits::default().protocol,
        )
        .unwrap(),
        diagnostic
    );

    block_on(host.dispatch_frame(
        [5; 16],
        5,
        Frame::Binary(unsubscribe()),
        &mut application,
    ))
    .unwrap();
    // The protocol defines terminal status without a Result body for a host
    // diagnostic, but is quiet about combining that with a closed watch. Keep
    // status terminal while replaying the original request/watch pair.
    let status = Envelope {
        request: Some([38; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: request_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let status = block_on(host.dispatch_frame(
        [5; 16],
        6,
        Frame::Binary(status),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("durable rejected request remains queryable");
    assert!(matches!(
        status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Terminal,
            fingerprint: Some(fingerprint),
            result: None,
        } if target == [37; 16] && fingerprint == request_fingerprint
    ));

    let mut issuer = Issuer(2, None);
    let (replacement, retired) = block_on(host.rotate_and_retire(
        [1; 16],
        &origin(),
        &credential,
        7,
        &mut issuer,
    ))
    .unwrap();
    assert_eq!(retired, Some([5; 16]));
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &replacement,
            attachment: [6; 16],
            now: 8,
        })),
        Ok(orna_security_v1::AttachOutcome::Reconnected)
    );

    let replay = block_on(host.dispatch_frame(
        [6; 16],
        9,
        Frame::Binary(request),
        &mut application,
    ))
    .unwrap();
    assert_eq!(replay.response, Some(diagnostic));
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_event_diagnostic_replays_after_host_recovery_without_restored_watch() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let old_owner = RequestOwner::from(block_on(runtime.acquire_lease([73; 16])).unwrap());
    let mut host = durable_host_with_owner(runtime, [73; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = WatchEventApplication {
        mode: WatchEventMode::Denied,
        subscriptions: 0,
        events: 0,
    };

    let fixture_eval = block_on(host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(eval_with_context([1; 16], [36; 16], [2; 16], None)),
        &mut application,
    ))
    .unwrap();
    assert!(matches!(
        fixture_eval.response.unwrap().message,
        Message::Diagnostic { .. }
    ));
    block_on(host.dispatch_frame(
        [5; 16],
        3,
        Frame::Binary(subscribe_request([35; 16])),
        &mut application,
    ))
    .unwrap();

    let request = event([1; 16], [37; 16], [11; 16]);
    let first = block_on(host.dispatch_frame(
        [5; 16],
        4,
        Frame::Binary(request.clone()),
        &mut application,
    ))
    .unwrap();
    let diagnostic = first.response.expect("portable rejection has a response");
    assert_eq!(diagnostic.request, Some([37; 16]));
    assert_eq!(diagnostic.watch, Some([11; 16]));
    assert!(matches!(diagnostic.message, Message::Diagnostic { .. }));
    let event_fingerprint = request_fingerprint(&request, [1; 16]);
    block_on(host.dispatch_frame(
        [5; 16],
        5,
        Frame::Binary(unsubscribe()),
        &mut application,
    ))
    .unwrap();
    drop(host);

    let recovery_runtime = open_durable_state(&repository);
    block_on(recovery_runtime.recover_abandoned(old_owner.owner_id, [74; 16])).unwrap();
    drop(recovery_runtime);

    // Rebuild process-local session/watch state, but retain the same durable
    // request identity. REQUEST-1 specifies terminal replay by fingerprint;
    // this host therefore returns the diagnostic before requiring watch restore.
    let mut recovered = durable_host_after_takeover(
        open_durable_state(&repository),
        [74; 16],
        old_owner,
    );
    let mut recovered_issuer = Issuer(2, None);
    let recovered_credential = create(&mut recovered, &mut recovered_issuer);
    block_on(recovered.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &recovered_credential,
        attachment: [6; 16],
        now: 6,
    }))
    .unwrap();
    let status_request = Envelope {
        request: Some([38; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let status = block_on(recovered.dispatch_frame(
        [6; 16],
        7,
        Frame::Binary(status_request),
        &mut UnitApplication::default(),
    ))
    .unwrap()
    .response
    .expect("recovered runtime exposes the rejected terminal status");
    assert!(matches!(
        status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Terminal,
            fingerprint: Some(fingerprint),
            result: None,
        } if target == [37; 16] && fingerprint == event_fingerprint
    ));

    let mut transport = LiveTransport::new(recovered, TransportLimits::default()).unwrap();
    let mut socket = WebSocketState::new([6; 16]);
    let mut application = UnitApplication::default();

    let mut stale_fingerprint = Envelope::decode(&request, Limits::default().protocol).unwrap();
    if let Message::Event { revision, .. } = &mut stale_fingerprint.message {
        *revision = 1;
    } else {
        panic!("fixture proof must retain an Event request");
    }
    let stale_fingerprint = stale_fingerprint
        .encode(Limits::default().protocol)
        .unwrap();
    let stale_diagnostic = block_on(transport.receive_with_application(
        &mut socket,
        8,
        &masked_binary_payload(&stale_fingerprint),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &stale_diagnostic[0] else {
        panic!("an invalid transmitted fingerprint gets a diagnostic");
    };
    let stale_diagnostic = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert_eq!(stale_diagnostic.request, Some([37; 16]));
    assert_eq!(stale_diagnostic.watch, None, "the old watch is not restored");
    assert!(matches!(
        &stale_diagnostic.message,
        Message::Diagnostic { .. }
    ));

    let mut altered = Envelope::decode(&request, Limits::default().protocol).unwrap();
    if let Message::Event { revision, .. } = &mut altered.message {
        *revision = 1;
    } else {
        panic!("fixture proof must retain an Event request");
    }
    let altered_fingerprint =
        canonical_request_fingerprint([1; 16], &altered, Limits::default().protocol).unwrap();
    if let Message::Event { fingerprint, .. } = &mut altered.message {
        *fingerprint = altered_fingerprint;
    }
    let altered = altered.encode(Limits::default().protocol).unwrap();

    // A status query with the competing canonical fingerprint must not reveal
    // the recovered terminal record under the old fingerprint.
    let competing_status_request = Envelope {
        request: Some([40; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: altered_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let competing_status_output = block_on(transport.receive_with_application(
        &mut socket,
        9,
        &masked_binary_payload(&competing_status_request),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &competing_status_output[0] else {
        panic!("a competing status fingerprint receives a portable diagnostic");
    };
    let competing_status = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert_eq!(competing_status.request, Some([40; 16]));
    assert_eq!(competing_status.watch, None);
    assert!(matches!(
        &competing_status.message,
        Message::Diagnostic { .. }
    ));

    // Reusing the rejected query ID for the corrected target fingerprint is
    // itself a request-fingerprint collision; it cannot rewrite that ID's
    // original portable rejection.
    let corrected_same_id = Envelope {
        request: Some([40; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let corrected_same_id = block_on(transport.receive_with_application(
        &mut socket,
        10,
        &masked_binary_payload(&corrected_same_id),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &corrected_same_id[0] else {
        panic!("a changed status payload cannot reuse the rejected query ID");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        competing_status
    );

    let exact_status_retry = block_on(transport.receive_with_application(
        &mut socket,
        11,
        &masked_binary_payload(&competing_status_request),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &exact_status_retry[0] else {
        panic!("an exact rejected status retry replays its diagnostic");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        competing_status
    );

    // A malformed retry is rejected before admission, so it cannot rewrite
    // the terminal record that RequestStatus exposes after host recovery.
    let post_rejection_status = Envelope {
        request: Some([39; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let post_rejection_status = block_on(transport.receive_with_application(
        &mut socket,
        12,
        &masked_binary_payload(&post_rejection_status),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &post_rejection_status[0] else {
        panic!("request status remains available after a stale frame");
    };
    let post_rejection_status = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert_eq!(post_rejection_status.request, Some([39; 16]));
    assert!(matches!(
        post_rejection_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Terminal,
            fingerprint: Some(fingerprint),
            result: None,
        } if target == [37; 16] && fingerprint == event_fingerprint
    ));

    let mismatch = block_on(transport.receive_with_application(
        &mut socket,
        13,
        &masked_binary_payload(&altered),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &mismatch[0] else {
        panic!("same request ID with a different fingerprint gets a diagnostic");
    };
    let mismatch = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert_eq!(mismatch.request, Some([37; 16]));
    assert_eq!(mismatch.watch, None, "the old watch is not restored");
    assert!(matches!(mismatch.message, Message::Diagnostic { .. }));

    let replay = block_on(transport.receive_with_application(
        &mut socket,
        14,
        &masked_binary_payload(&request),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &replay[0] else {
        panic!("an exact retry replays the durable Event diagnostic");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        diagnostic
    );
    assert_eq!(application.calls, 0);

    drop(transport);
    remove_test_repository(&root);
}

#[test]
fn stale_event_fingerprint_does_not_reserve_durable_request_id() {
    let (root, repository) = durable_repository();
    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut transport = LiveTransport::new(host, TransportLimits::default()).unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    let mut fixture_application = WatchEventApplication {
        mode: WatchEventMode::Pure,
        subscriptions: 0,
        events: 0,
    };

    let fixture_eval = eval_with_context([1; 16], [30; 16], [2; 16], None);
    let fixture_output = block_on(transport.receive_with_application(
        &mut socket,
        2,
        &masked_binary_payload(&fixture_eval),
        &mut fixture_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &fixture_output[0] else {
        panic!("the in-crate ORNA fixture receives a terminal response");
    };
    assert!(matches!(
        Envelope::decode(payload, Limits::default().protocol)
            .unwrap()
            .message,
        Message::Diagnostic { .. }
    ));

    let mut application = WatchEventApplication {
        mode: WatchEventMode::Denied,
        subscriptions: 0,
        events: 0,
    };

    let subscribe = subscribe_request([31; 16]);
    let subscribed = block_on(transport.receive_with_application(
        &mut socket,
        3,
        &masked_binary_payload(&subscribe),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &subscribed[0] else {
        panic!("subscription returns a snapshot");
    };
    let watch = Envelope::decode(payload, Limits::default().protocol)
        .unwrap()
        .watch
        .expect("the snapshot establishes the watch identity");

    let canonical_event = event([1; 16], [32; 16], watch);
    let mut stale = Envelope::decode(&canonical_event, Limits::default().protocol).unwrap();
    if let Message::Event { revision, .. } = &mut stale.message {
        *revision = 1;
    } else {
        panic!("fixture proof must retain an Event request");
    }
    let stale = stale.encode(Limits::default().protocol).unwrap();
    let rejected = block_on(transport.receive_with_application(
        &mut socket,
        4,
        &masked_binary_payload(&stale),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &rejected[0] else {
        panic!("stale embedded fingerprints receive a portable diagnostic");
    };
    let rejected = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert_eq!(rejected.request, Some([32; 16]));
    assert_eq!(rejected.watch, Some(watch));
    assert!(matches!(rejected.message, Message::Diagnostic { .. }));
    assert_eq!(application.events, 0, "the stale event never reaches the app");
    assert!(block_on(open_durable_state(&repository).request_status_for_identity(
        RequestIdentity {
            session_id: [1; 16],
            request_id: [32; 16],
        },
    ))
    .unwrap()
    .is_none(), "pre-admission rejection leaves the durable ID available");

    let accepted = block_on(transport.receive_with_application(
        &mut socket,
        5,
        &masked_binary_payload(&canonical_event),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &accepted[0] else {
        panic!("the corrected canonical retry completes");
    };
    let accepted = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert_eq!(accepted.request, Some([32; 16]));
    assert!(matches!(accepted.message, Message::Diagnostic { .. }));
    assert_eq!(application.events, 1, "only the corrected event is dispatched");

    let stored = block_on(open_durable_state(&repository).request_status_for_identity(
        RequestIdentity {
            session_id: [1; 16],
            request_id: [32; 16],
        },
    ))
    .unwrap()
    .expect("the corrected request is now durable");
    assert_eq!(stored.state, RequestState::Completed);
    assert_eq!(
        Envelope::decode(
            stored.terminal_outcome.as_ref().unwrap().as_bytes(),
            Limits::default().protocol,
        )
        .unwrap(),
        accepted
    );

    let replay = block_on(transport.receive_with_application(
        &mut socket,
        6,
        &masked_binary_payload(&canonical_event),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &replay[0] else {
        panic!("an exact canonical retry replays the terminal Event result");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        accepted
    );
    assert_eq!(application.events, 1, "terminal replay does not redispatch");

    drop(transport);
    remove_test_repository(&root);
}

#[test]
fn durable_status_retry_replays_unknown_after_target_completes_and_host_recovers() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let old_owner = RequestOwner::from(block_on(runtime.acquire_lease([75; 16])).unwrap());
    let mut host = durable_host_with_owner(runtime, [75; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut transport = LiveTransport::new(host, TransportLimits::default()).unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    let mut application = WatchEventApplication {
        mode: WatchEventMode::Denied,
        subscriptions: 0,
        events: 0,
    };

    let fixture_eval = eval_with_context([1; 16], [36; 16], [2; 16], None);
    let fixture_output = block_on(transport.receive_with_application(
        &mut socket,
        2,
        &masked_binary_payload(&fixture_eval),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &fixture_output[0] else {
        panic!("the in-crate ORNA fixture receives a host response");
    };
    assert!(matches!(
        Envelope::decode(payload, Limits::default().protocol)
            .unwrap()
            .message,
        Message::Diagnostic { .. }
    ));

    let subscribed = block_on(transport.receive_with_application(
        &mut socket,
        3,
        &masked_binary_payload(&subscribe_request([35; 16])),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &subscribed[0] else {
        panic!("the fixture-backed session establishes its watch");
    };
    let watch = Envelope::decode(payload, Limits::default().protocol)
        .unwrap()
        .watch
        .expect("subscription returns its watch identity");

    let event_request = event([1; 16], [37; 16], watch);
    let event_fingerprint = request_fingerprint(&event_request, [1; 16]);
    let unknown_status_request = Envelope {
        request: Some([38; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let unknown_status_output = block_on(transport.receive_with_application(
        &mut socket,
        4,
        &masked_binary_payload(&unknown_status_request),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &unknown_status_output[0] else {
        panic!("an unknown request returns a status snapshot");
    };
    let unknown_status = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert_eq!(unknown_status.request, Some([38; 16]));
    assert!(matches!(
        &unknown_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Unknown,
            fingerprint: None,
            result: None,
        } if *target == [37; 16]
    ));

    let event_output = block_on(transport.receive_with_application(
        &mut socket,
        5,
        &masked_binary_payload(&event_request),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &event_output[0] else {
        panic!("the event returns its terminal rejection");
    };
    let event_diagnostic = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert!(matches!(
        &event_diagnostic.message,
        Message::Diagnostic { .. }
    ));
    assert_eq!(application.events, 1);
    drop(transport);

    let recovery_runtime = open_durable_state(&repository);
    block_on(recovery_runtime.recover_abandoned(old_owner.owner_id, [76; 16])).unwrap();
    drop(recovery_runtime);
    let mut recovered = durable_host_after_takeover(
        open_durable_state(&repository),
        [76; 16],
        old_owner,
    );
    let mut recovered_issuer = Issuer(2, None);
    let recovered_credential = create(&mut recovered, &mut recovered_issuer);
    block_on(recovered.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &recovered_credential,
        attachment: [6; 16],
        now: 6,
    }))
    .unwrap();
    let mut recovered_transport =
        LiveTransport::new(recovered, TransportLimits::default()).unwrap();
    let mut recovered_socket = WebSocketState::new([6; 16]);
    let mut recovered_application = UnitApplication::default();

    let terminal_status_request = Envelope {
        request: Some([39; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let terminal_status_output = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        7,
        &masked_binary_payload(&terminal_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &terminal_status_output[0] else {
        panic!("a fresh status query observes the completed Event");
    };
    let terminal_status = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert!(matches!(
        terminal_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Terminal,
            fingerprint: Some(fingerprint),
            result: None,
        } if target == [37; 16] && fingerprint == event_fingerprint
    ));

    let unknown_retry = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        8,
        &masked_binary_payload(&unknown_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &unknown_retry[0] else {
        panic!("the exact status retry replays its original snapshot");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        unknown_status
    );

    let event_retry = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        9,
        &masked_binary_payload(&event_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &event_retry[0] else {
        panic!("the exact Event retry replays after process recovery");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        event_diagnostic
    );
    assert_eq!(recovered_application.calls, 0);

    drop(recovered_transport);
    remove_test_repository(&root);
}

#[test]
fn durable_request_status_identity_is_scoped_when_sessions_reuse_target_ids() {
    let (root, repository) = durable_repository();
    let mut host = durable_host(open_durable_state(&repository));

    let mut first_issuer = Issuer(1, None);
    let first_credential = create(&mut host, &mut first_issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &first_credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();

    let second_subscribe = subscribe();
    let mut second_issuer = Issuer(2, None);
    let second_credential = block_on(host.create(
        CreateRequest {
            id: [2; 16],
            origin: origin(),
            expires_at: 100,
            now: 0,
            subscribe: &second_subscribe,
        },
        &mut second_issuer,
    ))
    .unwrap();
    block_on(host.resume(ResumeRequest {
        id: [2; 16],
        origin: &origin(),
        credential: &second_credential,
        attachment: [6; 16],
        now: 1,
    }))
    .unwrap();

    // Both sessions admit the same fixture-backed request ID. Session identity
    // gives the requests distinct fingerprints and independently retained results.
    let first_target_request = eval_with_context([1; 16], [37; 16], [2; 16], None);
    let first_target_fingerprint = request_fingerprint(&first_target_request, [1; 16]);
    let second_target_request = eval_with_context([2; 16], [37; 16], [2; 16], None);
    let second_target_fingerprint = request_fingerprint(&second_target_request, [2; 16]);

    let mut application = UnitApplication::default();
    let first_target = block_on(host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(first_target_request),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the first session retains its fixture result");
    application.eval_outcome = UnitEvalOutcome::SemanticFailure;
    let second_target = block_on(host.dispatch_frame(
        [6; 16],
        2,
        Frame::Binary(second_target_request),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the second session retains its own fixture result");
    assert!(matches!(
        &first_target.message,
        Message::Result {
            status: ResultStatus::Success,
            ..
        }
    ));
    assert!(matches!(
        &second_target.message,
        Message::Result {
            status: ResultStatus::Failure,
            ..
        }
    ));

    let status_request = |request, fingerprint| {
        Envelope {
            request: Some(request),
            watch: None,
            message: Message::RequestStatus {
                target: [37; 16],
                fingerprint,
            },
            extensions: BTreeMap::new(),
        }
        .encode(Limits::default().protocol)
        .unwrap()
    };
    let first_status_request = status_request([44; 16], first_target_fingerprint);
    let second_status_request = status_request([44; 16], second_target_fingerprint);
    let first_status = block_on(host.dispatch_frame(
        [5; 16],
        3,
        Frame::Binary(first_status_request.clone()),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the first session reads its target status");
    let second_status = block_on(host.dispatch_frame(
        [6; 16],
        3,
        Frame::Binary(second_status_request.clone()),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the second session reads its target status");

    let first_result = ResultBody::from_result(&first_target, Limits::default().protocol).unwrap();
    let second_result =
        ResultBody::from_result(&second_target, Limits::default().protocol).unwrap();
    assert!(matches!(
        &first_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Terminal,
            fingerprint: Some(fingerprint),
            result: Some(result),
        } if *target == [37; 16]
            && *fingerprint == first_target_fingerprint
            && result == &first_result
    ));
    assert!(matches!(
        &second_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Terminal,
            fingerprint: Some(fingerprint),
            result: Some(result),
        } if *target == [37; 16]
            && *fingerprint == second_target_fingerprint
            && result == &second_result
    ));

    // A foreign target fingerprint is rejected in each session, and the
    // colliding query ID keeps each session's original status snapshot.
    for (attachment, foreign_fingerprint) in [
        ([5; 16], second_target_fingerprint),
        ([6; 16], first_target_fingerprint),
    ] {
        let mismatch = block_on(host.dispatch_frame(
            attachment,
            4,
            Frame::Binary(status_request([45; 16], foreign_fingerprint)),
            &mut application,
        ))
        .unwrap()
        .response
        .expect("a foreign target fingerprint returns a diagnostic");
        assert_eq!(mismatch.request, Some([45; 16]));
        assert!(matches!(mismatch.message, Message::Diagnostic { .. }));
    }

    let first_retry = block_on(host.dispatch_frame(
        [5; 16],
        5,
        Frame::Binary(first_status_request),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the first session replays its own status snapshot");
    let second_retry = block_on(host.dispatch_frame(
        [6; 16],
        5,
        Frame::Binary(second_status_request),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the second session replays its own status snapshot");
    assert_eq!(first_retry, first_status);
    assert_eq!(second_retry, second_status);
    assert_eq!(application.calls, 2);

    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_unknown_status_retry_survives_orphan_resolution() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let old_owner = RequestOwner::from(block_on(runtime.acquire_lease([77; 16])).unwrap());
    let mut host = durable_host_with_owner(runtime, [77; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut transport = LiveTransport::new(host, TransportLimits::default()).unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    let mut application = DeferredWatchEventApplication { events: 0 };

    let fixture_eval = eval_with_context([1; 16], [36; 16], [2; 16], None);
    let fixture_eval_fingerprint = request_fingerprint(&fixture_eval, [1; 16]);
    let fixture_output = block_on(transport.receive_with_application(
        &mut socket,
        2,
        &masked_binary_payload(&fixture_eval),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &fixture_output[0] else {
        panic!("the in-crate ORNA fixture receives a host response");
    };
    assert!(matches!(
        Envelope::decode(payload, Limits::default().protocol)
            .unwrap()
            .message,
        Message::Diagnostic { .. }
    ));

    let subscribe_request = subscribe_request([35; 16]);
    let subscribed = block_on(transport.receive_with_application(
        &mut socket,
        3,
        &masked_binary_payload(&subscribe_request),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &subscribed[0] else {
        panic!("the session establishes the Event watch");
    };
    let watch = Envelope::decode(payload, Limits::default().protocol)
        .unwrap()
        .watch
        .expect("subscription returns its watch identity");

    let event_request = event([1; 16], [37; 16], watch);
    let event_fingerprint = request_fingerprint(&event_request, [1; 16]);
    let unknown_status_request = Envelope {
        request: Some([38; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let unknown_output = block_on(transport.receive_with_application(
        &mut socket,
        4,
        &masked_binary_payload(&unknown_status_request),
        &mut application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &unknown_output[0] else {
        panic!("an unknown target returns a status snapshot");
    };
    let unknown_status = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert!(matches!(
        &unknown_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Unknown,
            fingerprint: None,
            result: None,
        } if *target == [37; 16]
    ));

    assert!(matches!(
        block_on(transport.receive_with_application(
            &mut socket,
            5,
            &masked_binary_payload(&event_request),
            &mut application,
        )),
        Err(Error::ApplicationDeferred)
    ));
    assert_eq!(application.events, 1);
    drop(transport);

    let recovery_runtime = open_durable_state(&repository);
    block_on(recovery_runtime.recover_abandoned(old_owner.owner_id, [78; 16])).unwrap();
    drop(recovery_runtime);
    let mut recovered = durable_host_after_takeover(
        open_durable_state(&repository),
        [78; 16],
        old_owner,
    );
    let mut recovered_issuer = Issuer(2, None);
    let recovered_credential = create(&mut recovered, &mut recovered_issuer);
    block_on(recovered.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &recovered_credential,
        attachment: [6; 16],
        now: 6,
    }))
    .unwrap();
    let mut second_issuer = Issuer(3, None);
    let second_subscribe = subscribe();
    let second_credential = block_on(recovered.create(
        CreateRequest {
            id: [2; 16],
            origin: origin(),
            expires_at: 100,
            now: 0,
            subscribe: &second_subscribe,
        },
        &mut second_issuer,
    ))
    .unwrap();
    block_on(recovered.resume(ResumeRequest {
        id: [2; 16],
        origin: &origin(),
        credential: &second_credential,
        attachment: [7; 16],
        now: 7,
    }))
    .unwrap();
    let mut recovered_transport =
        LiveTransport::new(recovered, TransportLimits::default()).unwrap();
    let mut recovered_socket = WebSocketState::new([6; 16]);
    let mut second_socket = WebSocketState::new([7; 16]);
    let mut recovered_application = UnitApplication::default();

    let orphan_status_request = Envelope {
        request: Some([39; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let orphan_status_output = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        7,
        &masked_binary_payload(&orphan_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &orphan_status_output[0] else {
        panic!("a fresh status query observes the recovered running Event");
    };
    let orphan_status = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert!(matches!(
        &orphan_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Orphaned,
            fingerprint: Some(fingerprint),
            ..
        } if *target == [37; 16] && *fingerprint == event_fingerprint
    ));

    // Recovery finalizes a still-running Event conservatively. Its exact
    // retry remains replayable even though the process-local watch was lost.
    let orphan_event_retry = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        8,
        &masked_binary_payload(&event_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &orphan_event_retry[0] else {
        panic!("the orphaned Event retry returns its recovered outcome");
    };
    let orphan_event_outcome = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert!(matches!(
        &orphan_event_outcome.message,
        Message::Result {
            status: ResultStatus::RetainedWithoutValue,
            value: None,
            fingerprint,
            diagnostic: None,
        } if *fingerprint == event_fingerprint
    ));
    assert_eq!(recovered_application.calls, 0);

    let advanced_status_request = Envelope {
        request: Some([40; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let advanced_status_output = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        9,
        &masked_binary_payload(&advanced_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &advanced_status_output[0] else {
        panic!("a new status query observes the retained orphan outcome");
    };
    let advanced_status = Envelope::decode(payload, Limits::default().protocol).unwrap();
    let expected_result = ResultBody::from_result(
        &orphan_event_outcome,
        Limits::default().protocol,
    )
    .unwrap();
    assert!(matches!(
        &advanced_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Orphaned,
            fingerprint: Some(fingerprint),
            result: Some(result),
        } if *target == [37; 16]
            && *fingerprint == event_fingerprint
            && result == &expected_result
    ));

    let mut conflicting_event =
        Envelope::decode(&event_request, Limits::default().protocol).unwrap();
    if let Message::Event { revision, .. } = &mut conflicting_event.message {
        *revision = 1;
    } else {
        panic!("the fixture proof must retain an Event request");
    }
    let conflicting_fingerprint = canonical_request_fingerprint(
        [1; 16],
        &conflicting_event,
        Limits::default().protocol,
    )
    .unwrap();
    let conflicting_status_request = Envelope {
        request: Some([41; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: conflicting_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let conflicting_status_output = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        10,
        &masked_binary_payload(&conflicting_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &conflicting_status_output[0] else {
        panic!("a competing fingerprint gets a portable mismatch diagnostic");
    };
    let conflicting_status = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert_eq!(conflicting_status.request, Some([41; 16]));
    assert!(matches!(
        &conflicting_status.message,
        Message::Diagnostic { .. }
    ));

    let corrected_status_request = Envelope {
        request: Some([41; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let corrected_status_output = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        11,
        &masked_binary_payload(&corrected_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &corrected_status_output[0] else {
        panic!("a rejected status query ID cannot be rebound");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        conflicting_status
    );

    let fresh_status_request = Envelope {
        request: Some([42; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let fresh_status_output = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        12,
        &masked_binary_payload(&fresh_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &fresh_status_output[0] else {
        panic!("a fresh query ID can read the correctly fingerprinted orphan");
    };
    let fresh_status = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert!(matches!(
        &fresh_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Orphaned,
            fingerprint: Some(fingerprint),
            result: Some(result),
        } if *target == [37; 16]
            && *fingerprint == event_fingerprint
            && result == &expected_result
    ));

    let conflicting_status_retry = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        13,
        &masked_binary_payload(&conflicting_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &conflicting_status_retry[0] else {
        panic!("the competing query ID keeps its original diagnostic");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        conflicting_status
    );

    let fresh_status_retry = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        14,
        &masked_binary_payload(&fresh_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &fresh_status_retry[0] else {
        panic!("the fresh corrected query ID keeps its orphan snapshot");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        fresh_status
    );

    // Both query IDs predate orphan resolution. A collision against either
    // bound snapshot must be fenced without replacing its exact replay.
    for (index, (query_id, original_request, original_snapshot)) in [
        ([38; 16], &unknown_status_request, &unknown_status),
        ([39; 16], &orphan_status_request, &orphan_status),
    ]
    .into_iter()
    .enumerate()
    {
        let collision_request = Envelope {
            request: Some(query_id),
            watch: None,
            message: Message::RequestStatus {
                target: [37; 16],
                fingerprint: conflicting_fingerprint,
            },
            extensions: BTreeMap::new(),
        }
        .encode(Limits::default().protocol)
        .unwrap();
        let sequence = 15 + index as u64 * 3;
        let collision_output = block_on(recovered_transport.receive_with_application(
            &mut recovered_socket,
            sequence,
            &masked_binary_payload(&collision_request),
            &mut recovered_application,
        ))
        .unwrap();
        let WebSocketOutput::Binary { payload, .. } = &collision_output[0] else {
            panic!("a reused snapshot ID with a competing fingerprint is rejected");
        };
        let collision_diagnostic = Envelope::decode(payload, Limits::default().protocol).unwrap();
        assert_eq!(collision_diagnostic.request, Some(query_id));
        assert!(matches!(
            &collision_diagnostic.message,
            Message::Diagnostic { .. }
        ));

        let collision_retry = block_on(recovered_transport.receive_with_application(
            &mut recovered_socket,
            sequence + 1,
            &masked_binary_payload(&collision_request),
            &mut recovered_application,
        ))
        .unwrap();
        let WebSocketOutput::Binary { payload, .. } = &collision_retry[0] else {
            panic!("the exact collision retry replays its diagnostic");
        };
        assert_eq!(
            Envelope::decode(payload, Limits::default().protocol).unwrap(),
            collision_diagnostic
        );

        let original_retry = block_on(recovered_transport.receive_with_application(
            &mut recovered_socket,
            sequence + 2,
            &masked_binary_payload(original_request),
            &mut recovered_application,
        ))
        .unwrap();
        let WebSocketOutput::Binary { payload, .. } = &original_retry[0] else {
            panic!("the original status snapshot remains replayable after collision");
        };
        assert_eq!(
            Envelope::decode(payload, Limits::default().protocol).unwrap(),
            *original_snapshot
        );
    }

    // The query identity is bound to its complete target pair. Reusing a
    // prior snapshot ID for a different target request is fenced as well.
    for (index, (query_id, target, original_request, original_snapshot)) in [
        ([38; 16], [50; 16], &unknown_status_request, &unknown_status),
        ([39; 16], [51; 16], &orphan_status_request, &orphan_status),
    ]
    .into_iter()
    .enumerate()
    {
        let collision_request = Envelope {
            request: Some(query_id),
            watch: None,
            message: Message::RequestStatus {
                target,
                fingerprint: event_fingerprint,
            },
            extensions: BTreeMap::new(),
        }
        .encode(Limits::default().protocol)
        .unwrap();
        let sequence = 21 + index as u64 * 3;
        let collision_output = block_on(recovered_transport.receive_with_application(
            &mut recovered_socket,
            sequence,
            &masked_binary_payload(&collision_request),
            &mut recovered_application,
        ))
        .unwrap();
        let WebSocketOutput::Binary { payload, .. } = &collision_output[0] else {
            panic!("a prior query ID cannot be rebound to another target");
        };
        let collision_diagnostic = Envelope::decode(payload, Limits::default().protocol).unwrap();
        assert_eq!(collision_diagnostic.request, Some(query_id));
        assert!(matches!(
            &collision_diagnostic.message,
            Message::Diagnostic { .. }
        ));

        let collision_retry = block_on(recovered_transport.receive_with_application(
            &mut recovered_socket,
            sequence + 1,
            &masked_binary_payload(&collision_request),
            &mut recovered_application,
        ))
        .unwrap();
        let WebSocketOutput::Binary { payload, .. } = &collision_retry[0] else {
            panic!("the changed-target retry replays its mismatch diagnostic");
        };
        assert_eq!(
            Envelope::decode(payload, Limits::default().protocol).unwrap(),
            collision_diagnostic
        );

        let original_retry = block_on(recovered_transport.receive_with_application(
            &mut recovered_socket,
            sequence + 2,
            &masked_binary_payload(original_request),
            &mut recovered_application,
        ))
        .unwrap();
        let WebSocketOutput::Binary { payload, .. } = &original_retry[0] else {
            panic!("the original snapshot survives a changed-target collision");
        };
        assert_eq!(
            Envelope::decode(payload, Limits::default().protocol).unwrap(),
            *original_snapshot
        );
    }

    // A different target may already have a durable terminal response. A
    // colliding prior query must not disclose that response or rewrite its snapshot.
    for (index, (query_id, original_request, original_snapshot)) in [
        ([38; 16], &unknown_status_request, &unknown_status),
        ([39; 16], &orphan_status_request, &orphan_status),
    ]
    .into_iter()
    .enumerate()
    {
        let collision_request = Envelope {
            request: Some(query_id),
            watch: None,
            message: Message::RequestStatus {
                target: [36; 16],
                fingerprint: fixture_eval_fingerprint,
            },
            extensions: BTreeMap::new(),
        }
        .encode(Limits::default().protocol)
        .unwrap();
        let sequence = 27 + index as u64 * 3;
        let collision_output = block_on(recovered_transport.receive_with_application(
            &mut recovered_socket,
            sequence,
            &masked_binary_payload(&collision_request),
            &mut recovered_application,
        ))
        .unwrap();
        let WebSocketOutput::Binary { payload, .. } = &collision_output[0] else {
            panic!("a query collision cannot disclose another terminal target");
        };
        let collision_diagnostic = Envelope::decode(payload, Limits::default().protocol).unwrap();
        assert_eq!(collision_diagnostic.request, Some(query_id));
        assert!(matches!(
            &collision_diagnostic.message,
            Message::Diagnostic { .. }
        ));

        let collision_retry = block_on(recovered_transport.receive_with_application(
            &mut recovered_socket,
            sequence + 1,
            &masked_binary_payload(&collision_request),
            &mut recovered_application,
        ))
        .unwrap();
        let WebSocketOutput::Binary { payload, .. } = &collision_retry[0] else {
            panic!("the terminal-target collision retry replays its diagnostic");
        };
        assert_eq!(
            Envelope::decode(payload, Limits::default().protocol).unwrap(),
            collision_diagnostic
        );

        let original_retry = block_on(recovered_transport.receive_with_application(
            &mut recovered_socket,
            sequence + 2,
            &masked_binary_payload(original_request),
            &mut recovered_application,
        ))
        .unwrap();
        let WebSocketOutput::Binary { payload, .. } = &original_retry[0] else {
            panic!("the prior snapshot survives a terminal-target collision");
        };
        assert_eq!(
            Envelope::decode(payload, Limits::default().protocol).unwrap(),
            *original_snapshot
        );
    }

    // A RequestStatus query cannot borrow the orphan target's own request ID:
    // that durable identity remains bound to the Event and its recovered result.
    let self_target_status_request = Envelope {
        request: Some([37; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let self_target_status_output = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        33,
        &masked_binary_payload(&self_target_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &self_target_status_output[0] else {
        panic!("a status query reusing the target ID receives a collision diagnostic");
    };
    let self_target_status = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert_eq!(self_target_status.request, Some([37; 16]));
    assert!(matches!(
        &self_target_status.message,
        Message::Diagnostic { .. }
    ));

    let self_target_status_retry = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        34,
        &masked_binary_payload(&self_target_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &self_target_status_retry[0] else {
        panic!("the target-ID collision retry replays its diagnostic");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        self_target_status
    );

    // Request and target IDs are scoped by session. Reusing the same query
    // bytes across sessions may therefore observe different target snapshots.
    let cross_session_status_request = Envelope {
        request: Some([44; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let second_session_status_output = block_on(recovered_transport.receive_with_application(
        &mut second_socket,
        35,
        &masked_binary_payload(&cross_session_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &second_session_status_output[0] else {
        panic!("the other session has an independent target snapshot");
    };
    let second_session_status = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert!(matches!(
        &second_session_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Unknown,
            fingerprint: None,
            result: None,
        } if *target == [37; 16]
    ));

    let first_session_status_output = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        36,
        &masked_binary_payload(&cross_session_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &first_session_status_output[0] else {
        panic!("the owning session still sees its orphaned target");
    };
    let first_session_status = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert!(matches!(
        &first_session_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Orphaned,
            fingerprint: Some(fingerprint),
            result: Some(result),
        } if *target == [37; 16]
            && *fingerprint == event_fingerprint
            && result == &expected_result
    ));

    let second_session_status_retry = block_on(recovered_transport.receive_with_application(
        &mut second_socket,
        37,
        &masked_binary_payload(&cross_session_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &second_session_status_retry[0] else {
        panic!("the second session replays its own Unknown snapshot");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        second_session_status
    );

    let first_session_status_retry = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        38,
        &masked_binary_payload(&cross_session_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &first_session_status_retry[0] else {
        panic!("the owning session replays its Orphaned snapshot");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        first_session_status
    );

    let post_collision_status_request = Envelope {
        request: Some([43; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [37; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let post_collision_status_output = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        39,
        &masked_binary_payload(&post_collision_status_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &post_collision_status_output[0] else {
        panic!("a fresh status ID still observes the orphaned Event");
    };
    assert!(matches!(
        &Envelope::decode(payload, Limits::default().protocol)
            .unwrap()
            .message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Orphaned,
            fingerprint: Some(fingerprint),
            result: Some(_),
        } if *target == [37; 16] && *fingerprint == event_fingerprint
    ));

    let final_event_retry = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        40,
        &masked_binary_payload(&event_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &final_event_retry[0] else {
        panic!("the Event identity remains replayable after its ID collision");
    };
    assert!(matches!(
        &Envelope::decode(payload, Limits::default().protocol)
            .unwrap()
            .message,
        Message::Result {
            status: ResultStatus::RetainedWithoutValue,
            value: None,
            fingerprint,
            diagnostic: None,
        } if *fingerprint == event_fingerprint
    ));

    // The same numeric request ID may name a query in another session even
    // while it remains the orphaned Event identity in the owning session.
    let scoped_collision_request = Envelope {
        request: Some([37; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [50; 16],
            fingerprint: event_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let second_session_scoped_output = block_on(recovered_transport.receive_with_application(
        &mut second_socket,
        41,
        &masked_binary_payload(&scoped_collision_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &second_session_scoped_output[0] else {
        panic!("the second session has its own request identity for ID 37");
    };
    let second_session_scoped_status =
        Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert!(matches!(
        &second_session_scoped_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Unknown,
            fingerprint: None,
            result: None,
        } if *target == [50; 16]
    ));

    let first_session_scoped_output = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        42,
        &masked_binary_payload(&scoped_collision_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &first_session_scoped_output[0] else {
        panic!("the first session fences ID 37 as its Event identity");
    };
    let first_session_scoped_diagnostic =
        Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert_eq!(first_session_scoped_diagnostic.request, Some([37; 16]));
    assert!(matches!(
        &first_session_scoped_diagnostic.message,
        Message::Diagnostic { .. }
    ));

    let second_session_scoped_retry = block_on(recovered_transport.receive_with_application(
        &mut second_socket,
        43,
        &masked_binary_payload(&scoped_collision_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &second_session_scoped_retry[0] else {
        panic!("the second session replays its own query snapshot");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        second_session_scoped_status
    );

    let first_session_scoped_retry = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        44,
        &masked_binary_payload(&scoped_collision_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &first_session_scoped_retry[0] else {
        panic!("the owning session replays its Event-ID collision diagnostic");
    };
    assert_eq!(
        Envelope::decode(payload, Limits::default().protocol).unwrap(),
        first_session_scoped_diagnostic
    );

    let post_scope_event_retry = block_on(recovered_transport.receive_with_application(
        &mut recovered_socket,
        45,
        &masked_binary_payload(&event_request),
        &mut recovered_application,
    ))
    .unwrap();
    let WebSocketOutput::Binary { payload, .. } = &post_scope_event_retry[0] else {
        panic!("the owning session's orphaned Event remains replayable");
    };
    assert!(matches!(
        &Envelope::decode(payload, Limits::default().protocol)
            .unwrap()
            .message,
        Message::Result {
            status: ResultStatus::RetainedWithoutValue,
            value: None,
            fingerprint,
            diagnostic: None,
        } if *fingerprint == event_fingerprint
    ));
    assert_eq!(recovered_application.calls, 0);

    drop(recovered_transport);
    remove_test_repository(&root);
}

#[test]
fn websocket_commit_without_completed_delivery_aborts_candidate_and_preserves_incumbent() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(r#"{{"database":"{}","protocol":"orna.present.v1"}}"#, uuid(2)),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let session_token = token(&created);
    let active = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &session_token), [5; 16], 1)
        .unwrap();
    assert_eq!(commit_delivered(&mut transport, active, 1).unwrap().status, 101);

    let candidate = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &session_token), [6; 16], 2)
        .unwrap();
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(candidate, 2)),
        Err(Error::Closed)
    );
    assert_eq!(transport.take_retired_attachments(), vec![[6; 16]]);
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(1, &session_token), [5; 16], 3)
            .unwrap_err()
            .status,
        503,
        "the active incumbent must remain attached after an undelivered commit"
    );
}

#[test]
fn websocket_upgrade_reservation_blocks_only_its_session_until_commit() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut deletion = Delete(true);
    let first = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let first_token = token(&first);
    let second = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(3)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let second_token = token(&second);

    let active = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &first_token), [5; 16], 1)
        .unwrap();
    assert_eq!(
        commit_delivered(&mut transport, active, 1)
            .unwrap()
            .status,
        101
    );

    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &first_token), [6; 16], 2)
        .unwrap();
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(1, &first_token), [7; 16], 2)
            .unwrap_err()
            .status,
        503
    );
    assert_eq!(
        block_on(transport.handle(
            wire(
                "POST",
                "/orna/session/01010101-0101-0101-0101-010101010101/resume",
                &format!(r#"{{"resume_token":"{first_token}","protocol":"orna.present.v1"}}"#),
            ),
            2,
            &mut authority,
            &mut issuer,
            &mut deletion,
        ))
        .status,
        503
    );
    assert_eq!(
        block_on(transport.handle(
            wire(
                "POST",
                "/orna/session/02020202-0202-0202-0202-020202020202/resume",
                &format!(r#"{{"resume_token":"{second_token}","protocol":"orna.present.v1"}}"#),
            ),
            2,
            &mut authority,
            &mut issuer,
            &mut deletion,
        ))
        .status,
        200
    );
    assert_eq!(
        commit_delivered(&mut transport, pending, 2)
            .unwrap()
            .status,
        101
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
}

#[test]
fn websocket_upgrade_rejects_attachment_identity_owned_by_another_handoff() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut deletion = Delete(true);
    let first = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let second = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(3)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let first_token = token(&first);
    let second_token = token(&second);

    let active = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &first_token), [5; 16], 1)
        .unwrap();
    commit_delivered(&mut transport, active, 1).unwrap();
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(2, &second_token), [5; 16], 2)
            .unwrap_err()
            .status,
        503
    );

    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &first_token), [6; 16], 2)
        .unwrap();
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(2, &second_token), [6; 16], 2)
            .unwrap_err()
            .status,
        503
    );
    assert!(transport.abort_websocket_upgrade(&pending));
    assert_eq!(transport.take_retired_attachments(), vec![[6; 16]]);
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(2, &second_token), [6; 16], 3)
            .unwrap_err()
            .status,
        503
    );
    assert!(transport.acknowledge_retired_attachment([6; 16]));

    assert_eq!(
        block_on(transport.upgrade(websocket_upgrade(2, &second_token), [6; 16], 3)).status,
        101
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([6; 16]),
            3,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([5; 16]),
            3,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
}

#[test]
fn websocket_upgrade_abort_preserves_attachment_and_consumes_reservation() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let credential = token(&created);

    let mut malformed = websocket_upgrade(1, &credential);
    malformed
        .headers
        .retain(|(name, _)| name != "sec-websocket-protocol");
    assert_eq!(
        transport
            .begin_websocket_upgrade(&malformed, [5; 16], 1)
            .unwrap_err()
            .status,
        400
    );
    let active = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [5; 16], 1)
        .unwrap();
    assert_eq!(
        commit_delivered(&mut transport, active, 1)
            .unwrap()
            .status,
        101
    );

    let aborted = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 2)
        .unwrap();
    assert!(transport.abort_websocket_upgrade(&aborted));
    assert!(!transport.abort_websocket_upgrade(&aborted));
    assert_eq!(transport.take_retired_attachments(), vec![[6; 16]]);
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 3)
            .unwrap_err()
            .status,
        503
    );
    assert!(transport.acknowledge_retired_attachment([6; 16]));
    assert!(!transport.acknowledge_retired_attachment([6; 16]));
    assert_eq!(
        block_on(transport.upgrade(websocket_upgrade(1, &credential), [6; 16], 3)).status,
        101
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
    // The acknowledged candidate identity is reusable by a later committed
    // handoff, but its consumed reservation must remain inert. In particular,
    // a stale terminal replay cannot consume or retire the new attachment.
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(aborted, 4)),
        Err(Error::Closed)
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([6; 16]),
            4,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
    assert_eq!(transport.take_retired_attachments(), Vec::<[u8; 16]>::new());
}

#[test]
fn websocket_upgrade_expiry_queues_and_fences_candidate_without_replacing_attachment() {
    let limits = TransportLimits {
        lease_ms: 10,
        request_retention_ms: 10,
        ..TransportLimits::default()
    };
    let mut transport = LiveTransport::new(host(), limits).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let credential = token(&created);
    let second = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(3)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let second_token = token(&second);
    let active = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [5; 16], 1)
        .unwrap();
    assert_eq!(
        commit_delivered(&mut transport, active, 1)
            .unwrap()
            .status,
        101
    );

    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 2)
        .unwrap();
    transport.expire_pending_websocket_upgrades(12);
    assert_eq!(transport.take_retired_attachments(), vec![[6; 16]]);
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 12)
            .unwrap_err()
            .status,
        503
    );
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(2, &second_token), [6; 16], 12)
            .unwrap_err()
            .status,
        503
    );
    assert!(transport.acknowledge_retired_attachment([6; 16]));
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(pending, 12)),
        Err(Error::Closed)
    );
    assert_eq!(
        block_on(transport.upgrade(websocket_upgrade(2, &second_token), [6; 16], 13)).status,
        101
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([6; 16]),
            13,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
    assert!(
        block_on(transport.receive(
            &mut WebSocketState::new([5; 16]),
            13,
            &masked(true, 2, &unsubscribe()),
        ))
        .is_ok()
    );
    // The expired candidate was retired and acknowledged above. This
    // cross-session admission never replaced the original attachment, so it
    // has no additional worker to retire.
    assert_eq!(transport.take_retired_attachments(), Vec::<[u8; 16]>::new());
}

#[test]
fn foreign_upgrade_reservation_cannot_consume_a_local_pending_handshake() {
    let mut first = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut second = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut first_issuer = Issuer(1, None);
    let mut second_issuer = Issuer(1, None);
    let mut first_authority = Authority;
    let mut second_authority = Authority;
    let mut first_deletion = Delete(true);
    let mut second_deletion = Delete(true);
    let body = format!(
        r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
        uuid(2)
    );
    let first_created = block_on(first.handle(
        wire("POST", "/orna/session", &body),
        0,
        &mut first_authority,
        &mut first_issuer,
        &mut first_deletion,
    ));
    let second_created = block_on(second.handle(
        wire("POST", "/orna/session", &body),
        0,
        &mut second_authority,
        &mut second_issuer,
        &mut second_deletion,
    ));
    let first_pending = first
        .begin_websocket_upgrade(&websocket_upgrade(1, &token(&first_created)), [5; 16], 1)
        .unwrap();
    let second_pending = second
        .begin_websocket_upgrade(&websocket_upgrade(1, &token(&second_created)), [5; 16], 1)
        .unwrap();

    assert!(!second.abort_websocket_upgrade(&first_pending));
    assert_eq!(
        block_on(second.commit_websocket_upgrade(first_pending, 1)),
        Err(Error::Closed)
    );
    assert_eq!(
        second
            .begin_websocket_upgrade(&websocket_upgrade(1, &token(&second_created)), [6; 16], 1)
            .unwrap_err()
            .status,
        503
    );
    assert_eq!(
        commit_delivered(&mut second, second_pending, 1)
            .unwrap()
            .status,
        101
    );
}

#[test]
fn foreign_expired_reservation_commit_cannot_expire_local_pending_handshake() {
    let limits = TransportLimits {
        lease_ms: 10,
        request_retention_ms: 10,
        ..TransportLimits::default()
    };
    let mut first = LiveTransport::new(host(), limits).unwrap();
    let mut second = LiveTransport::new(host(), limits).unwrap();
    let mut first_issuer = Issuer(1, None);
    let mut second_issuer = Issuer(1, None);
    let mut first_authority = Authority;
    let mut second_authority = Authority;
    let mut first_deletion = Delete(true);
    let mut second_deletion = Delete(true);
    let body = format!(
        r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
        uuid(2)
    );
    let first_created = block_on(first.handle(
        wire("POST", "/orna/session", &body),
        0,
        &mut first_authority,
        &mut first_issuer,
        &mut first_deletion,
    ));
    let second_created = block_on(second.handle(
        wire("POST", "/orna/session", &body),
        0,
        &mut second_authority,
        &mut second_issuer,
        &mut second_deletion,
    ));
    let first_pending = first
        .begin_websocket_upgrade(&websocket_upgrade(1, &token(&first_created)), [5; 16], 1)
        .unwrap();
    let second_pending = second
        .begin_websocket_upgrade(&websocket_upgrade(1, &token(&second_created)), [5; 16], 1)
        .unwrap();

    // Ownership is checked before expiry cleanup: a foreign actor cannot use
    // an expired-looking reservation to mutate a local actor's admission.
    assert_eq!(
        block_on(second.commit_websocket_upgrade(first_pending, 11)),
        Err(Error::Closed)
    );
    assert!(second.take_retired_attachments().is_empty());

    // The owning actor still performs the normal expiry transition and fences
    // its candidate before that stale reservation can be used again.
    second.expire_pending_websocket_upgrades(11);
    assert_eq!(second.take_retired_attachments(), vec![[5; 16]]);
    assert_eq!(
        block_on(second.commit_websocket_upgrade(second_pending, 11)),
        Err(Error::Closed)
    );
}

#[test]
fn consumed_reservation_replay_cannot_expire_another_pending_handshake() {
    let limits = TransportLimits {
        lease_ms: 10,
        request_retention_ms: 10,
        ..TransportLimits::default()
    };
    let mut transport = LiveTransport::new(host(), limits).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut deletion = Delete(true);
    let first = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let second = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(3)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let stale = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &token(&first)), [5; 16], 1)
        .unwrap();
    assert!(transport.abort_websocket_upgrade(&stale));
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
    assert!(transport.acknowledge_retired_attachment([5; 16]));

    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(2, &token(&second)), [6; 16], 1)
        .unwrap();

    // The token is stale before the second candidate's delivery window has
    // been considered.  Its replay cannot run the expiry sweep or publish a
    // retirement for that unrelated candidate.
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(stale, 11)),
        Err(Error::Closed)
    );
    assert!(transport.take_retired_attachments().is_empty());

    transport.expire_pending_websocket_upgrades(11);
    assert_eq!(transport.take_retired_attachments(), vec![[6; 16]]);
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(pending, 11)),
        Err(Error::Closed)
    );
}

#[test]
fn child_aware_delete_retires_active_and_pending_websocket_candidates() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let mut children = RecordingChildren::default();
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let credential = token(&created);
    let active = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [5; 16], 1)
        .unwrap();
    commit_delivered(&mut transport, active, 1).unwrap();
    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 2)
        .unwrap();
    let mut request = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    request
        .headers
        .push(("authorization".into(), format!("Bearer {credential}")));
    assert_eq!(
        block_on(transport.handle_with_children(
            request,
            3,
            &mut authority,
            &mut issuer,
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16], [6; 16]]);
    assert_eq!(
        block_on(transport.commit_websocket_upgrade(pending, 3)),
        Err(Error::Closed)
    );
}

#[test]
fn cleanup_acknowledgement_requires_a_delivered_retirement_notice() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let credential = token(&created);
    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [5; 16], 1)
        .unwrap();
    assert!(transport.abort_websocket_upgrade(&pending));

    assert!(!transport.acknowledge_retired_attachment([5; 16]));
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [5; 16], 3)
            .unwrap_err()
            .status,
        503
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
    assert!(transport.acknowledge_retired_attachment([5; 16]));
}

#[test]
fn child_free_delete_preserves_session_until_socket_cleanup_can_be_joined() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = RecordingDelete::default();
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let credential = token(&created);
    let active = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [5; 16], 1)
        .unwrap();
    commit_delivered(&mut transport, active, 1).unwrap();
    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 2)
        .unwrap();
    let mut request = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    request
        .headers
        .push(("authorization".into(), format!("Bearer {credential}")));

    assert_eq!(
        block_on(transport.handle(request, 3, &mut authority, &mut issuer, &mut deletion,)).status,
        503
    );
    assert_eq!(deletion.calls, 0);
    assert!(transport.take_retired_attachments().is_empty());
    assert_eq!(
        commit_delivered(&mut transport, pending, 3)
            .unwrap()
            .status,
        101
    );
}

#[test]
fn rejected_delete_preserves_pending_websocket_admission() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let credential = token(&created);
    let pending = transport
        .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [6; 16], 1)
        .unwrap();
    let mut request = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    request.headers[0].1 = "https://other.example".into();
    request
        .headers
        .push(("authorization".into(), format!("Bearer {credential}")));
    assert_eq!(
        block_on(transport.handle(request, 2, &mut authority, &mut issuer, &mut deletion,)).status,
        403
    );
    assert_eq!(
        transport
            .begin_websocket_upgrade(&websocket_upgrade(1, &credential), [7; 16], 2)
            .unwrap_err()
            .status,
        503
    );
    assert!(transport.abort_websocket_upgrade(&pending));
}

#[test]
fn http_contract_has_stable_status_headers_and_redacted_errors() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let payload = subscribe();
    let create = block_on(host.http_create(
        CreateRequest {
            id: [1; 16],
            origin: origin(),
            expires_at: 100,
            now: 0,
            subscribe: &payload,
        },
        &mut issuer,
    ));
    assert_eq!(create.status, 201);
    assert_eq!(
        create.headers,
        vec![("content-type", "application/orna-live-v1")]
    );
    let HttpBody::Session(credential) = create.body else {
        panic!("session body expected");
    };
    let resume = block_on(host.http_resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }));
    assert_eq!(resume.status, 101);
    assert_eq!(
        resume.headers,
        vec![
            ("upgrade", "websocket"),
            ("sec-websocket-protocol", SUBPROTOCOL),
        ]
    );
    let origin = origin();
    let deleted = block_on(host.http_delete(
        DeleteRequest {
            id: [1; 16],
            origin: &origin,
            credential: &credential,
            now: 1,
        },
        &mut Delete(true),
    ));
    assert_eq!(deleted.status, 204);
    assert_eq!(deleted.body, HttpBody::Empty);
}

#[test]
#[allow(clippy::too_many_lines)]
fn delete_requires_current_unexpired_bearer_and_original_origin() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut authority = Authority;
    let mut issuer = Issuer(1, None);
    let mut deletion = RecordingDelete::default();
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(created.status, 201);
    let credential = token(&created);

    let mut wrong_origin = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    wrong_origin.headers[0].1 = "https://other.example".into();
    wrong_origin
        .headers
        .push(("authorization".into(), format!("Bearer {credential}")));
    assert_eq!(
        block_on(transport.handle(wrong_origin, 1, &mut authority, &mut issuer, &mut deletion,))
            .status,
        403
    );
    assert_eq!(deletion.calls, 0);

    let resumed = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session/01010101-0101-0101-0101-010101010101/resume",
            &format!(r#"{{"resume_token":"{credential}","protocol":"orna.present.v1"}}"#),
        ),
        2,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(resumed.status, 200);
    let rotated = token(&resumed);

    let mut stale = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    stale
        .headers
        .push(("authorization".into(), format!("Bearer {credential}")));
    assert_eq!(
        block_on(transport.handle(stale, 3, &mut authority, &mut issuer, &mut deletion,)).status,
        410
    );
    assert_eq!(deletion.calls, 0);

    let mut cookie_only = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    cookie_only
        .headers
        .push(("cookie".into(), format!("orna_session={rotated}")));
    assert_eq!(
        block_on(transport.handle(cookie_only, 3, &mut authority, &mut issuer, &mut deletion,))
            .status,
        401
    );
    assert_eq!(deletion.calls, 0);

    let mut valid = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    valid
        .headers
        .push(("authorization".into(), format!("Bearer {rotated}")));
    assert_eq!(
        block_on(transport.handle(valid, 3, &mut authority, &mut issuer, &mut deletion,)).status,
        204
    );
    assert_eq!(deletion.calls, 1);

    let mut expired_transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut expired_authority = Authority;
    let mut expired_issuer = Issuer(1, None);
    let mut expired_deletion = RecordingDelete::default();
    let expired_created = block_on(expired_transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut expired_authority,
        &mut expired_issuer,
        &mut expired_deletion,
    ));
    let expired_credential = token(&expired_created);
    let mut expired = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    expired.headers.push((
        "authorization".into(),
        format!("Bearer {expired_credential}"),
    ));
    assert_eq!(
        block_on(expired_transport.handle(
            expired,
            100,
            &mut expired_authority,
            &mut expired_issuer,
            &mut expired_deletion,
        ))
        .status,
        410
    );
    assert_eq!(expired_deletion.calls, 0);
}

#[test]
fn frames_are_bounded_binary_canonical_and_cancellable() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    host.reserve_request([1; 16], [8; 16]).unwrap();
    host.start_request([1; 16], [8; 16]).unwrap();
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.handle_frame([5; 16], 2, Frame::Text("x".into()))),
        Err(Error::InvalidFrame)
    );
    assert_eq!(
        block_on(host.handle_frame([5; 16], 2, Frame::Binary(vec![0xff]))),
        Err(Error::InvalidMessage)
    );
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(cancel()), &mut application,))
            .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Cancelled)
    );
    assert_eq!(
        block_on(host.handle_frame([5; 16], 2, Frame::Binary(resync()))),
        Err(Error::Denied)
    );
    assert_eq!(application.calls, 1);
}

#[test]
fn decoded_server_result_is_rejected_before_durable_admission_or_application() {
    let (root, repository) = durable_repository();
    let mut host = durable_host_with_owner(open_durable_state(&repository), [87; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [88; 16],
        now: 1,
    }))
    .unwrap();

    let request = [89; 16];
    let result = unit_result(request, [0; 32])
        .encode(Limits::default().protocol)
        .unwrap();
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame([88; 16], 2, Frame::Binary(result), &mut application,)),
        Err(Error::InvalidMessage)
    );
    assert_eq!(application.calls, 0);
    assert!(
        block_on(
            open_durable_state(&repository).request_status_for_identity(RequestIdentity {
                session_id: [1; 16],
                request_id: request,
            })
        )
        .unwrap()
        .is_none()
    );

    drop(host);
    remove_test_repository(&root);
}

#[test]
fn expired_attachment_rejects_a_valid_binary_request_before_admission() {
    let (root, repository) = durable_repository();
    let mut host = durable_host_with_owner(open_durable_state(&repository), [86; 16]);
    let mut issuer = Issuer(1, None);
    let credential = block_on(host.create(
        CreateRequest {
            id: [1; 16],
            origin: origin(),
            expires_at: 3,
            now: 0,
            subscribe: &subscribe(),
        },
        &mut issuer,
    ))
    .unwrap();
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [85; 16],
        now: 1,
    }))
    .unwrap();

    let request = [84; 16];
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame(
            [85; 16],
            3,
            Frame::Binary(eval([1; 16], request, "1")),
            &mut application,
        )),
        Err(Error::Closed)
    );
    assert_eq!(application.calls, 0);
    assert!(
        block_on(
            open_durable_state(&repository).request_status_for_identity(RequestIdentity {
                session_id: [1; 16],
                request_id: request,
            })
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        block_on(host.dispatch_frame(
            [85; 16],
            4,
            Frame::Binary(eval([1; 16], request, "1")),
            &mut application,
        )),
        Err(Error::Closed)
    );

    drop(host);
    remove_test_repository(&root);
}

#[test]
fn replaced_attachment_cannot_admit_a_frame_or_disconnect_its_replacement() {
    let (root, repository) = durable_repository();
    let mut host = durable_host_with_owner(open_durable_state(&repository), [83; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [82; 16],
        now: 1,
    }))
    .unwrap();
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [81; 16],
        now: 2,
    }))
    .unwrap();

    let stale_request = [80; 16];
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame(
            [82; 16],
            3,
            Frame::Binary(eval([1; 16], stale_request, "1")),
            &mut application,
        )),
        Err(Error::Closed)
    );
    assert_eq!(application.calls, 0);
    assert!(
        block_on(
            open_durable_state(&repository).request_status_for_identity(RequestIdentity {
                session_id: [1; 16],
                request_id: stale_request,
            })
        )
        .unwrap()
        .is_none()
    );

    assert_eq!(
        block_on(host.dispatch_frame(
            [81; 16],
            3,
            Frame::Binary(eval([1; 16], [79; 16], "1")),
            &mut application,
        ))
        .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Accepted)
    );
    assert_eq!(application.calls, 1);

    drop(host);
    remove_test_repository(&root);
}

#[test]
fn rejected_non_durable_cancellation_callback_does_not_cancel_the_target() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    host.reserve_request([1; 16], [8; 16]).unwrap();
    host.start_request([1; 16], [8; 16]).unwrap();
    let mut rejecting = UnitApplication {
        reject_cancel: true,
        ..UnitApplication::default()
    };
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(cancel()), &mut rejecting,)),
        Err(Error::ApplicationRejected)
    );
    let mut accepting = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame(
            [5; 16],
            3,
            Frame::Binary(cancel_request([13; 16], [8; 16])),
            &mut accepting,
        ))
        .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Cancelled)
    );
    assert_eq!(accepting.calls, 1);
}

#[test]
fn durable_public_request_helpers_reject_without_serving_mutation() {
    let (root, repository) = durable_repository();
    let mut host = durable_host_with_owner(open_durable_state(&repository), [69; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [11; 16],
        now: 1,
    }))
    .unwrap();
    assert_eq!(
        host.reserve_request([1; 16], [68; 16]),
        Err(Error::UnsupportedOperation)
    );
    assert_eq!(
        host.start_request([1; 16], [68; 16]),
        Err(Error::UnsupportedOperation)
    );

    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame(
            [11; 16],
            2,
            Frame::Binary(eval([1; 16], [68; 16], "1")),
            &mut application,
        ))
        .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Accepted)
    );
    assert_eq!(application.calls, 1);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_stale_event_is_rejected_before_request_admission() {
    let (root, repository) = durable_repository();
    let mut host = durable_host_with_owner(open_durable_state(&repository), [70; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [12; 16],
        now: 1,
    }))
    .unwrap();

    let request = [71; 16];
    let frame = event([1; 16], request, [72; 16]);
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame([12; 16], 2, Frame::Binary(frame), &mut application,)),
        Err(Error::Denied)
    );
    assert_eq!(application.calls, 0);
    assert!(
        block_on(
            open_durable_state(&repository).request_status_for_identity(RequestIdentity {
                session_id: [1; 16],
                request_id: request,
            })
        )
        .unwrap()
        .is_none()
    );

    drop(host);
    remove_test_repository(&root);
}

#[test]
fn dispatch_computes_fingerprints_and_replays_terminal_results() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let first = eval([1; 16], [20; 16], "1");
    let replay =
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(first.clone()), &mut application))
            .unwrap();
    assert_eq!(application.calls, 1);
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(first), &mut application,)),
        Ok(replay.clone())
    );
    assert_eq!(application.calls, 1);
    let different_input = eval([1; 16], [20; 16], "2");
    assert_eq!(
        block_on(
            host.dispatch_frame([5; 16], 2, Frame::Binary(different_input), &mut application,)
        ),
        Err(Error::RequestMismatch)
    );
    assert_eq!(application.calls, 1);
}

impl LiveApplication for DeferredWatchEventApplication {
    fn eval(
        &mut self,
        _: [u8; 16],
        _: [u8; 16],
        _: &Message,
    ) -> Result<Envelope, Error> {
        Err(Error::UnsupportedOperation)
    }

    fn watch(&mut self, _: [u8; 16], _: [u8; 16], _: &Message) -> Result<Envelope, Error> {
        Err(Error::UnsupportedOperation)
    }

    fn subscribe(
        &mut self,
        _: [u8; 16],
        request: [u8; 16],
        _: &Message,
    ) -> Result<Envelope, Error> {
        Ok(WatchEventApplication::snapshot(request, [11; 16]))
    }

    fn dispatch_event_with_work<'a>(
        &'a mut self,
        _: [u8; 16],
        _: [u8; 16],
        message: &'a Message,
        _: Option<[u8; 16]>,
        _: [u8; 32],
        _: Option<&'a orna_runtime_v1::RuntimeActivationContext>,
        _: &'a mut orna_live_v1::LiveApplicationWorkLease,
    ) -> Pin<Box<dyn Future<Output = Result<LiveEvalResponse, Error>> + 'a>> {
        if !matches!(message, Message::Event { .. }) {
            return Box::pin(async { Err(Error::ApplicationRejected) });
        }
        self.events += 1;
        Box::pin(async { Err(Error::ApplicationDeferred) })
    }
}

#[test]
fn subscribe_request_identity_replays_only_the_original_watch() {
    // ORNA-PROTO-002 retains a session's terminal request outcomes; a new
    // ORNA-WIRE-006 resubscription therefore needs a fresh request identity.
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = WatchEventApplication {
        mode: WatchEventMode::Pure,
        subscriptions: 0,
        events: 0,
    };

    let original_request = subscribe_request([41; 16]);
    let original = block_on(host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(original_request.clone()),
        &mut application,
    ))
    .unwrap();
    assert_eq!(application.subscriptions, 1);
    assert!(matches!(
        original.response.as_ref().map(|response| &response.message),
        Some(Message::Snapshot { revision: 0, .. })
    ));

    let replay = block_on(host.dispatch_frame(
        [5; 16],
        3,
        Frame::Binary(original_request),
        &mut application,
    ))
    .unwrap();
    assert_eq!(replay, original);
    assert_eq!(application.subscriptions, 1);

    let resubscription = block_on(host.dispatch_frame(
        [5; 16],
        4,
        Frame::Binary(subscribe_request([42; 16])),
        &mut application,
    ))
    .unwrap();
    assert_eq!(application.subscriptions, 2);
    assert!(matches!(
        resubscription.response.as_ref().map(|response| &response.message),
        Some(Message::Snapshot { revision: 0, .. })
    ));
    assert_ne!(
        resubscription.response.unwrap().watch,
        original.response.unwrap().watch
    );
}

#[test]
fn watch_resync_replaces_the_complete_identity_bearing_tree_at_a_new_revision() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = IdentityWatchApplication {
        next_revision: 1,
        next_present: IdentityWatchApplication::present("refreshed"),
    };

    let initial = block_on(host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(subscribe_request([81; 16])),
        &mut application,
    ))
    .unwrap();
    let initial_response = initial.response.unwrap();
    assert_eq!(initial_response.watch, Some([55; 16]));
    assert!(matches!(
        initial_response.message,
        Message::Snapshot { revision: 0, present, .. }
            if present == IdentityWatchApplication::present("initial")
    ));

    let refreshed = block_on(host.dispatch_frame(
        [5; 16],
        3,
        Frame::Binary(resync_request([82; 16], [55; 16])),
        &mut application,
    ))
    .unwrap();
    assert_eq!(refreshed.outcome, FrameOutcome::Resync { revisions: 0 });
    let refreshed_response = refreshed.response.unwrap();
    assert_eq!(refreshed_response.watch, Some([55; 16]));
    assert!(matches!(
        refreshed_response.message,
        Message::Snapshot { revision: 1, present, .. }
            if present == IdentityWatchApplication::present("refreshed")
    ));
}

#[test]
fn watch_resync_rejects_changed_same_revision_and_regressed_snapshots() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = IdentityWatchApplication {
        next_revision: 0,
        next_present: IdentityWatchApplication::present("changed without revision"),
    };
    block_on(host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(subscribe_request([83; 16])),
        &mut application,
    ))
    .unwrap();

    assert_eq!(
        block_on(host.dispatch_frame(
            [5; 16],
            3,
            Frame::Binary(resync_request([84; 16], [55; 16])),
            &mut application,
        )),
        Err(Error::ApplicationRejected)
    );

    application.next_revision = 1;
    application.next_present = IdentityWatchApplication::present("revision one");
    block_on(host.dispatch_frame(
        [5; 16],
        4,
        Frame::Binary(resync_request([85; 16], [55; 16])),
        &mut application,
    ))
    .unwrap();

    application.next_revision = 0;
    application.next_present = IdentityWatchApplication::present("regressed");
    assert_eq!(
        block_on(host.dispatch_frame(
            [5; 16],
            5,
            Frame::Binary(resync_request([86; 16], [55; 16])),
            &mut application,
        )),
        Err(Error::ApplicationRejected)
    );
}

#[test]
fn rejected_requests_retain_failure_identity_and_do_not_reexecute() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication {
        reject: true,
        ..UnitApplication::default()
    };
    let first = eval([1; 16], [21; 16], "1");
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(first.clone()), &mut application,)),
        Err(Error::ApplicationRejected)
    );
    assert_eq!(application.calls, 1);
    assert_eq!(
        block_on(host.dispatch_frame(
            [5; 16],
            2,
            Frame::Binary(eval([1; 16], [21; 16], "2")),
            &mut application,
        )),
        Err(Error::RequestMismatch)
    );
    let replay =
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(first), &mut application)).unwrap();
    assert!(matches!(
        replay.response.unwrap().message,
        Message::Result {
            status: ResultStatus::Failure,
            value: None,
            ..
        }
    ));
    assert_eq!(application.calls, 1);
}

#[test]
fn durable_transactional_eval_commits_or_rolls_back_and_replays_terminally() {
    let (root, repository) = durable_repository();
    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();

    let mut committed = TransactionalApplication {
        calls: 0,
        faults: Arc::new(NoFault),
        mutations: vec![
            TableMutation::new([1; 16], "books", vec![1], Some(vec![2])).unwrap(),
        ],
    };
    let request = eval([1; 16], [23; 16], "insert");
    let first = block_on(host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(request.clone()),
        &mut committed,
    ))
    .unwrap();
    assert!(matches!(
        first.response.as_ref().unwrap().message,
        Message::Result {
            status: ResultStatus::Success,
            fingerprint,
            ..
        } if fingerprint == request_fingerprint(&request, [1; 16])
    ));
    assert_eq!(committed.calls, 1);
    assert_eq!(
        block_on(open_durable_state(&repository).committed_table_row("books", &[1])),
        Ok(Some(vec![2]))
    );
    let status = block_on(open_durable_state(&repository).request_status_for_identity(
        RequestIdentity {
            session_id: [1; 16],
            request_id: [23; 16],
        },
    ))
    .unwrap()
    .unwrap();
    assert_eq!(status.state, RequestState::Completed);
    let replay = block_on(host.dispatch_frame(
        [5; 16],
        3,
        Frame::Binary(request),
        &mut committed,
    ))
    .unwrap();
    assert_eq!(replay, first);
    assert_eq!(committed.calls, 1);

    let failed_request = eval([1; 16], [24; 16], "rollback");
    let mut failed = TransactionalApplication {
        calls: 0,
        faults: Arc::new(FailAt(FaultPoint::AfterTableWrite)),
        mutations: vec![
            TableMutation::new([2; 16], "books", vec![2], Some(vec![3])).unwrap(),
        ],
    };
    assert_eq!(
        block_on(host.dispatch_frame(
            [5; 16],
            4,
            Frame::Binary(failed_request.clone()),
            &mut failed,
        )),
        Err(Error::RuntimeUnavailable)
    );
    assert_eq!(failed.calls, 1);
    assert_eq!(
        block_on(open_durable_state(&repository).committed_table_row("books", &[2])),
        Ok(None)
    );
    let failed_status = block_on(open_durable_state(&repository).request_status_for_identity(
        RequestIdentity {
            session_id: [1; 16],
            request_id: [24; 16],
        },
    ))
    .unwrap()
    .unwrap();
    assert_eq!(failed_status.state, RequestState::Completed);
    let mut replay_application = TransactionalApplication {
        calls: 0,
        faults: Arc::new(NoFault),
        mutations: vec![
            TableMutation::new([9; 16], "books", vec![9], Some(vec![9])).unwrap(),
        ],
    };

    let failed_replay = block_on(host.dispatch_frame(
        [5; 16],
        5,
        Frame::Binary(failed_request),
        &mut replay_application,
    ))
    .unwrap();
    assert!(matches!(
        failed_replay.response.as_ref().unwrap().message,
        Message::Result {
            status: ResultStatus::Failure,
            ..
        }
    ));
    assert_eq!(replay_application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_transactional_event_commits_through_the_application_ticket() {
    let (root, repository) = durable_repository();
    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();

    let mut application = TransactionalApplication {
        calls: 0,
        faults: Arc::new(NoFault),
        mutations: vec![
            TableMutation::new([6; 16], "event_books", vec![1], Some(vec![2])).unwrap(),
        ],
    };
    block_on(host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(subscribe()),
        &mut application,
    ))
    .unwrap();
    let request = event([1; 16], [23; 16], [11; 16]);
    let preparation = block_on(host.prepare_application_frame(
        [5; 16],
        3,
        Frame::Binary(request.clone()),
    ))
    .unwrap();
    let ticket = match preparation {
        orna_live_v1::ApplicationPreparation::Work(ticket) => ticket,
        orna_live_v1::ApplicationPreparation::Completed(_) => {
            panic!("event should reach the application worker")
        }
    };
    let completion = block_on(ticket.execute(&mut application));
    let outcome = block_on(host.complete_application(completion)).unwrap();
    assert!(matches!(
        outcome.response.as_ref().map(|response| &response.message),
        Some(Message::Result {
            status: ResultStatus::Success,
            ..
        })
    ));
    assert_eq!(application.calls, 1);
    assert_eq!(
        block_on(open_durable_state(&repository).committed_table_row("event_books", &[1])),
        Ok(Some(vec![2]))
    );
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_transactional_event_commits_through_synchronous_dispatch() {
    let (root, repository) = durable_repository();
    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();

    let mut application = TransactionalApplication {
        calls: 0,
        faults: Arc::new(NoFault),
        mutations: vec![
            TableMutation::new([7; 16], "event_books_sync", vec![1], Some(vec![2])).unwrap(),
        ],
    };
    block_on(host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(subscribe()),
        &mut application,
    ))
    .unwrap();
    let request = event([1; 16], [24; 16], [11; 16]);
    let outcome = block_on(host.dispatch_frame(
        [5; 16],
        3,
        Frame::Binary(request),
        &mut application,
    ))
    .unwrap();
    assert!(matches!(
        outcome.response.as_ref().map(|response| &response.message),
        Some(Message::Result {
            status: ResultStatus::Success,
            ..
        })
    ));
    assert_eq!(application.calls, 1);
    assert_eq!(
        block_on(open_durable_state(&repository).committed_table_row("event_books_sync", &[1])),
        Ok(Some(vec![2]))
    );
    drop(host);
    remove_test_repository(&root);
}
#[test]
fn mutating_event_invalidates_only_its_owning_watch() {
    let (root, repository) = durable_repository();
    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = WatchEventApplication {
        mode: WatchEventMode::Commit,
        subscriptions: 0,
        events: 0,
    };
    for (request, attachment_sequence) in [([30; 16], 2), ([31; 16], 3)] {
        block_on(host.dispatch_frame(
            [5; 16],
            attachment_sequence,
            Frame::Binary(subscribe_request(request)),
            &mut application,
        ))
        .unwrap();
    }

    let outcome = block_on(host.dispatch_frame(
        [5; 16],
        4,
        Frame::Binary(event([1; 16], [32; 16], [11; 16])),
        &mut application,
    ))
    .unwrap();
    assert!(matches!(
        outcome.response.as_ref().map(|response| &response.message),
        Some(Message::Result {
            status: ResultStatus::Success,
            ..
        })
    ));
    assert_eq!(
        block_on(host.dispatch_frame(
            [5; 16],
            5,
            Frame::Binary(resync_request([33; 16], [11; 16])),
            &mut application,
        )),
        Err(Error::Denied)
    );
    let other_watch = block_on(host.dispatch_frame(
        [5; 16],
        6,
        Frame::Binary(resync_request([34; 16], [12; 16])),
        &mut application,
    ))
    .unwrap();
    assert!(matches!(other_watch.outcome, FrameOutcome::Resync { .. }));
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn pure_rejected_and_rolled_back_events_preserve_their_watch() {
    for mode in [
        WatchEventMode::Pure,
        WatchEventMode::Rejected,
        WatchEventMode::Rollback,
    ] {
        let (root, repository) = durable_repository();
        let mut host = durable_host(open_durable_state(&repository));
        let mut issuer = Issuer(1, None);
        let credential = create(&mut host, &mut issuer);
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [5; 16],
            now: 1,
        }))
        .unwrap();
        let mut application = WatchEventApplication {
            mode,
            subscriptions: 0,
            events: 0,
        };
        block_on(host.dispatch_frame(
            [5; 16],
            2,
            Frame::Binary(subscribe_request([35; 16])),
            &mut application,
        ))
        .unwrap();

        let event_result = block_on(host.dispatch_frame(
            [5; 16],
            3,
            Frame::Binary(event([1; 16], [36; 16], [11; 16])),
            &mut application,
        ));
        match mode {
            WatchEventMode::Pure => assert!(event_result.is_ok()),
            WatchEventMode::Rejected => assert_eq!(event_result, Err(Error::ApplicationRejected)),
            WatchEventMode::Denied => assert!(event_result.is_ok()),
            WatchEventMode::Rollback => assert_eq!(event_result, Err(Error::RuntimeUnavailable)),
            WatchEventMode::Commit => unreachable!("commit is covered by the invalidation test"),
        }
        let resync = block_on(host.dispatch_frame(
            [5; 16],
            4,
            Frame::Binary(resync_request([37; 16], [11; 16])),
            &mut application,
        ))
        .unwrap();
        assert!(matches!(resync.outcome, FrameOutcome::Resync { .. }));
        drop(host);
        remove_test_repository(&root);
    }
}

#[test]
fn cancelled_event_preserves_its_watch() {
    let (root, repository) = durable_repository();
    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = WatchEventApplication {
        mode: WatchEventMode::Commit,
        subscriptions: 0,
        events: 0,
    };
    block_on(host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(subscribe_request([38; 16])),
        &mut application,
    ))
    .unwrap();

    let mut target = match block_on(host.prepare_application_frame(
        [5; 16],
        3,
        Frame::Binary(event([1; 16], [39; 16], [11; 16])),
    ))
    .unwrap()
    {
        orna_live_v1::ApplicationPreparation::Work(ticket) => ticket,
        orna_live_v1::ApplicationPreparation::Completed(_) => {
            panic!("event should remain pending until cancellation")
        }
    };
    let cancel = host.prepare_application_frame(
        [5; 16],
        4,
        Frame::Binary(cancel_request([40; 16], [39; 16])),
    );
    let release_target = async {
        assert!(target.cancellation().await.is_ok());
        assert!(target.is_cancelled());
        let _ = target.reject(Error::Closed);
    };
    let (cancelled, ()) = block_on(async { futures::join!(cancel, release_target) });
    match cancelled.unwrap() {
        orna_live_v1::ApplicationPreparation::Work(ticket) => {
            let _ = ticket.reject(Error::Closed);
        }
        orna_live_v1::ApplicationPreparation::Completed(_) => {
            panic!("cancellation should reach the application boundary")
        }
    }

    let resync = block_on(host.dispatch_frame(
        [5; 16],
        5,
        Frame::Binary(resync_request([41; 16], [11; 16])),
        &mut application,
    ))
    .unwrap();
    assert!(matches!(resync.outcome, FrameOutcome::Resync { .. }));
    drop(host);
    remove_test_repository(&root);
}

struct RegistryActionHandler {
    calls: Arc<AtomicUsize>,
}

impl ActionHandler for RegistryActionHandler {
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Ok(LiveEvalResponse::pure(Envelope {
                request: Some(request),
                watch: None,
                message: Message::Result {
                    status: ResultStatus::Success,
                    value: Some(CanonicalValue::unit()),
                    fingerprint,
                    diagnostic: None,
                },
                extensions: BTreeMap::new(),
            }))
        })
    }
}

#[test]
fn public_action_registry_is_consumable_without_server_dependency() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let context = block_on(runtime.begin_activation()).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let binding = ActionBinding {
        session: [1; 16],
        watch: [2; 16],
        page_revision: 7,
        action: [3; 16],
    };
    let mut registry = ActionAuthorityRegistry::new();
    registry
        .register(
            binding,
            RegistryActionHandler {
                calls: Arc::clone(&calls),
            },
        )
        .unwrap();

    let invoke = |registry: &ActionAuthorityRegistry,
                  binding: ActionBinding,
                  value: CanonicalValue,
                  request: [u8; 16]|
     -> Result<LiveEvalResponse, Error> {
        let supervisor = LiveApplicationWorkSupervisor::new();
        let mut work = supervisor.admit(binding.session, request)?;
        block_on(registry.activate(
            binding,
            request,
            [8; 32],
            &value,
            &context,
            &mut work,
        ))
    };

    for wrong in [
        (
            ActionBinding {
                session: [4; 16],
                ..binding
            },
            CanonicalValue::unit(),
            [11; 16],
        ),
        (
            ActionBinding {
                watch: [5; 16],
                ..binding
            },
            CanonicalValue::unit(),
            [12; 16],
        ),
        (
            ActionBinding {
                page_revision: 8,
                ..binding
            },
            CanonicalValue::unit(),
            [13; 16],
        ),
        (binding, CanonicalValue::uuid([9; 16]), [14; 16]),
    ] {
        assert!(matches!(
            invoke(&registry, wrong.0, wrong.1, wrong.2),
            Err(Error::Denied)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    assert!(matches!(
        invoke(&registry, binding, CanonicalValue::unit(), [15; 16]),
        Ok(LiveEvalResponse::Pure(_))
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_replays_a_terminal_request_after_host_reconstruction() {
    let (root, repository) = durable_repository();
    let mut first_host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut first_host, &mut issuer);
    block_on(first_host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let request = eval([1; 16], [23; 16], "1");
    let mut first_application = UnitApplication::default();
    let first = block_on(first_host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(request.clone()),
        &mut first_application,
    ))
    .unwrap();
    assert_eq!(first_application.calls, 1);
    let first_response_bytes = first
        .response
        .as_ref()
        .unwrap()
        .encode(Limits::default().protocol)
        .unwrap();
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [23; 16],
    };
    let retained_terminal_bytes =
        block_on(open_durable_state(&repository).request_status_for_identity(identity))
            .unwrap()
            .unwrap()
            .terminal_outcome
            .unwrap()
            .as_bytes()
            .to_vec();
    assert_eq!(first_response_bytes, retained_terminal_bytes);
    let observations = block_on(open_durable_state(&repository).run_observations()).unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].invocation_id, [23; 16]);
    assert_eq!(observations[0].status, RunObservationStatus::Completed);
    assert!(!observations[0].live);
    drop(first_host);

    let mut second_host = durable_host(open_durable_state(&repository));
    let mut second_issuer = Issuer(1, None);
    let second_credential = create(&mut second_host, &mut second_issuer);
    block_on(second_host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &second_credential,
        attachment: [6; 16],
        now: 3,
    }))
    .unwrap();
    let mut second_application = UnitApplication::default();
    let replay = block_on(second_host.dispatch_frame(
        [6; 16],
        4,
        Frame::Binary(request),
        &mut second_application,
    ))
    .unwrap();
    let replay_response_bytes = replay
        .response
        .as_ref()
        .unwrap()
        .encode(Limits::default().protocol)
        .unwrap();
    assert_eq!(replay, first);
    assert_eq!(replay_response_bytes, retained_terminal_bytes);
    assert_eq!(second_application.calls, 0);
    assert_eq!(first_application.calls + second_application.calls, 1);
    assert_eq!(
        block_on(second_host.dispatch_frame(
            [6; 16],
            4,
            Frame::Binary(eval([1; 16], [23; 16], "2")),
            &mut second_application,
        )),
        Err(Error::RequestMismatch)
    );
    drop(second_host);
    remove_test_repository(&root);
}

#[allow(clippy::too_many_lines)]
fn assert_durable_application_result_replays_verbatim(
    mut first_application: UnitApplication,
    request_id: [u8; 16],
    expected_status: ResultStatus,
) {
    let (root, repository) = durable_repository();
    let mut first_host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut first_host, &mut issuer);
    block_on(first_host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();

    let request = eval([1; 16], request_id, "1");
    let first = block_on(first_host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(request.clone()),
        &mut first_application,
    ))
    .unwrap();
    assert!(matches!(
        first.response.as_ref().unwrap().message,
        Message::Result { status, .. } if status == expected_status
    ));
    assert_eq!(first_application.calls, 1);
    let first_response_bytes = first
        .response
        .as_ref()
        .unwrap()
        .encode(Limits::default().protocol)
        .unwrap();
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id,
    };
    let durable_status =
        block_on(open_durable_state(&repository).request_status_for_identity(identity))
            .unwrap()
            .unwrap();
    assert_eq!(
        durable_status.state,
        orna_runtime_v1::RequestState::Completed
    );
    let retained_terminal_bytes = durable_status.terminal_outcome.unwrap().as_bytes().to_vec();
    assert_eq!(first_response_bytes, retained_terminal_bytes);

    let replay = block_on(first_host.dispatch_frame(
        [5; 16],
        2,
        Frame::Binary(request.clone()),
        &mut first_application,
    ))
    .unwrap();
    let replay_response_bytes = replay
        .response
        .as_ref()
        .unwrap()
        .encode(Limits::default().protocol)
        .unwrap();
    assert_eq!(replay_response_bytes, retained_terminal_bytes);
    assert_eq!(first_application.calls, 1);
    drop(first_host);

    let mut reconstructed_host = durable_host(open_durable_state(&repository));
    let mut reconstructed_issuer = Issuer(1, None);
    let reconstructed_credential = create(&mut reconstructed_host, &mut reconstructed_issuer);
    block_on(reconstructed_host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &reconstructed_credential,
        attachment: [6; 16],
        now: 3,
    }))
    .unwrap();
    let mut reconstructed_application = UnitApplication::default();
    let reconstructed_replay = block_on(reconstructed_host.dispatch_frame(
        [6; 16],
        4,
        Frame::Binary(request),
        &mut reconstructed_application,
    ))
    .unwrap();
    let reconstructed_replay_bytes = reconstructed_replay
        .response
        .as_ref()
        .unwrap()
        .encode(Limits::default().protocol)
        .unwrap();
    assert_eq!(reconstructed_replay_bytes, retained_terminal_bytes);
    assert_eq!(reconstructed_application.calls, 0);
    assert_eq!(first_application.calls + reconstructed_application.calls, 1);
    let reconstructed_status =
        block_on(open_durable_state(&repository).request_status_for_identity(identity))
            .unwrap()
            .unwrap();
    assert_eq!(
        reconstructed_status.state,
        orna_runtime_v1::RequestState::Completed
    );
    assert_eq!(
        reconstructed_status.terminal_outcome.unwrap().as_bytes(),
        retained_terminal_bytes
    );
    drop(reconstructed_host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_retains_and_replays_a_correlated_semantic_failure_result() {
    assert_durable_application_result_replays_verbatim(
        UnitApplication {
            eval_outcome: UnitEvalOutcome::SemanticFailure,
            ..UnitApplication::default()
        },
        [28; 16],
        ResultStatus::Failure,
    );
}

#[test]
fn durable_runtime_retains_and_replays_a_unit_success_result() {
    assert_durable_application_result_replays_verbatim(
        UnitApplication::default(),
        [29; 16],
        ResultStatus::Success,
    );
}

#[test]
fn durable_live_rejection_marks_its_observed_run_failed() {
    let (root, repository) = durable_repository();
    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication {
        reject: true,
        ..UnitApplication::default()
    };
    let request = eval([1; 16], [24; 16], "1");
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(request), &mut application)),
        Err(Error::ApplicationRejected)
    );
    let observations = block_on(open_durable_state(&repository).run_observations()).unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].invocation_id, [24; 16]);
    assert_eq!(observations[0].status, RunObservationStatus::Failed);
    assert!(observations[0].diagnostic.is_some());
    assert!(!observations[0].live);
    drop(host);
    remove_test_repository(&root);
}

#[test]
#[allow(clippy::too_many_lines)]
fn durable_request_status_recovers_states_and_enforces_target_fingerprint() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let states = [
        ([31; 16], [41; 32], orna_protocol_v1::RequestState::Reserved),
        ([32; 16], [42; 32], orna_protocol_v1::RequestState::Running),
        ([33; 16], [43; 32], orna_protocol_v1::RequestState::Terminal),
        ([34; 16], [44; 32], orna_protocol_v1::RequestState::Terminal),
        ([35; 16], [45; 32], orna_protocol_v1::RequestState::Orphaned),
    ];
    for (target, fingerprint, _) in states {
        let identity = RequestIdentity {
            session_id: [1; 16],
            request_id: target,
        };
        block_on(runtime.reserve_request(identity, fingerprint)).unwrap();
        if target != [31; 16] {
            block_on(runtime.start_request(identity, fingerprint)).unwrap();
        }
        let terminal = if target == [35; 16] {
            TerminalOutcome::new(
                Envelope {
                    request: Some(target),
                    watch: None,
                    message: Message::Result {
                        status: ResultStatus::RetainedWithoutValue,
                        value: None,
                        fingerprint,
                        diagnostic: None,
                    },
                    extensions: BTreeMap::new(),
                }
                .encode(Limits::default().protocol)
                .unwrap(),
            )
            .unwrap()
        } else if target == [33; 16] {
            TerminalOutcome::new(
                unit_result(target, fingerprint)
                    .encode(Limits::default().protocol)
                    .unwrap(),
            )
            .unwrap()
        } else if target == [34; 16] {
            TerminalOutcome::new(
                Envelope {
                    request: Some(target),
                    watch: None,
                    message: Message::Result {
                        status: ResultStatus::Cancellation,
                        value: None,
                        fingerprint,
                        diagnostic: None,
                    },
                    extensions: BTreeMap::new(),
                }
                .encode(Limits::default().protocol)
                .unwrap(),
            )
            .unwrap()
        } else {
            TerminalOutcome::new(Vec::new()).unwrap()
        };
        match target[0] {
            33 => {
                block_on(runtime.complete_request(identity, fingerprint, terminal)).unwrap();
            }
            34 => {
                block_on(runtime.cancel_request(identity, fingerprint, terminal)).unwrap();
            }
            35 => {
                let old = block_on(runtime.acquire_lease([91; 16])).unwrap();
                let fence = block_on(runtime.recover_abandoned(old.owner_id, [92; 16])).unwrap();
                block_on(runtime.recover_legacy_running_request(
                    identity,
                    fingerprint,
                    fence,
                    terminal,
                ))
                .unwrap();
            }
            _ => {}
        }
    }
    drop(runtime);

    let mut host = durable_host_after_takeover(
        open_durable_state(&repository),
        [92; 16],
        RequestOwner {
            owner_id: [91; 16],
            epoch: 1,
        },
    );
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [6; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    for (request, (target, fingerprint, state)) in
        [[51; 16], [52; 16], [53; 16], [54; 16], [55; 16]]
            .into_iter()
            .zip(states)
    {
        let status_request = Envelope {
            request: Some(request),
            watch: None,
            message: Message::RequestStatus {
                target,
                fingerprint,
            },
            extensions: BTreeMap::new(),
        }
        .encode(Limits::default().protocol)
        .unwrap();
        let outcome = block_on(host.dispatch_frame(
            [6; 16],
            2,
            Frame::Binary(status_request),
            &mut application,
        ))
        .unwrap();
        let expected_state = if state == orna_protocol_v1::RequestState::Running {
            orna_protocol_v1::RequestState::Orphaned
        } else {
            state
        };
        assert!(matches!(
            outcome.response.unwrap().message,
            Message::RequestStatusResult {
                target: returned_target,
                state: returned_state,
                fingerprint: Some(returned_fingerprint),
                result,
            } if returned_target == target
                && returned_state == expected_state
                && returned_fingerprint == fingerprint
                && result.is_some() == (target != [31; 16])
        ));
    }
    let mismatch = Envelope {
        request: Some([61; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [31; 16],
            fingerprint: [0; 32],
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let mismatch_outcome =
        block_on(host.dispatch_frame([6; 16], 2, Frame::Binary(mismatch), &mut application))
            .unwrap();
    assert_eq!(mismatch_outcome.outcome, FrameOutcome::Accepted);
    let response = mismatch_outcome
        .response
        .expect("request mismatch response");
    let diagnostic = FoundationDiagnostic::new(
        SafeText::new(Error::RequestMismatch.code()).unwrap(),
        DiagnosticSeverity::Error,
        SafeText::redacted(),
    )
    .unwrap()
    .redacted()
    .with_reference([61; 16]);
    let diagnostic = Value::decode(&diagnostic.encode_ovb().unwrap())
        .unwrap()
        .raw()
        .clone();
    let expected = Value::new(OvbRaw::Map(vec![
        (OvbRaw::Int(0.into()), OvbRaw::Int(1.into())),
        (OvbRaw::Int(1.into()), OvbRaw::Int(19.into())),
        (OvbRaw::Int(2.into()), OvbRaw::Bytes([61; 16].to_vec())),
        (OvbRaw::Int(3.into()), OvbRaw::Null),
        (
            OvbRaw::Int(4.into()),
            OvbRaw::Map(vec![(OvbRaw::Int(0.into()), diagnostic)]),
        ),
    ]))
    .unwrap()
    .encode()
    .unwrap();
    assert_eq!(
        response,
        Envelope::decode(&expected, Limits::default().protocol).unwrap()
    );

    let fresh_status = Envelope {
        request: Some([62; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [31; 16],
            fingerprint: [41; 32],
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let fresh_response =
        block_on(host.dispatch_frame([6; 16], 2, Frame::Binary(fresh_status), &mut application))
            .unwrap()
            .response
            .expect("fresh request status response");
    assert!(matches!(
        fresh_response.message,
        Message::RequestStatusResult {
            target,
            state,
            fingerprint,
            result,
        } if target == [31; 16]
            && state == orna_protocol_v1::RequestState::Reserved
            && fingerprint == Some([41; 32])
            && result.is_none()
    ));
    // RequestStatus only reads durable state: neither active rows nor
    // retained terminal rows may invoke the application while reporting it.
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
#[allow(clippy::too_many_lines)]
fn durable_request_status_keeps_duplicate_targets_session_scoped_after_recovery() {
    let (root, repository) = durable_repository();
    let first_target_request = eval_with_context([1; 16], [37; 16], [2; 16], None);
    let first_target_fingerprint = request_fingerprint(&first_target_request, [1; 16]);
    let second_target_request = eval_with_context([2; 16], [37; 16], [2; 16], None);
    let second_target_fingerprint = request_fingerprint(&second_target_request, [2; 16]);
    assert_ne!(first_target_fingerprint, second_target_fingerprint);

    let runtime = open_durable_state(&repository);
    for (session_id, fingerprint, response) in [
        (
            [1; 16],
            first_target_fingerprint,
            unit_result([37; 16], first_target_fingerprint),
        ),
        (
            [2; 16],
            second_target_fingerprint,
            semantic_failure_result([37; 16], second_target_fingerprint),
        ),
    ] {
        let identity = RequestIdentity {
            session_id,
            request_id: [37; 16],
        };
        block_on(runtime.reserve_request(identity, fingerprint)).unwrap();
        block_on(runtime.start_request(identity, fingerprint)).unwrap();
        let terminal = TerminalOutcome::new(
            response.encode(Limits::default().protocol).unwrap(),
        )
        .unwrap();
        block_on(runtime.complete_request(identity, fingerprint, terminal)).unwrap();
    }
    let old_owner = RequestOwner::from(block_on(runtime.acquire_lease([91; 16])).unwrap());
    drop(runtime);

    let recovery_runtime = open_durable_state(&repository);
    block_on(recovery_runtime.recover_abandoned(old_owner.owner_id, [92; 16])).unwrap();
    drop(recovery_runtime);
    let mut host = durable_host_after_takeover(
        open_durable_state(&repository),
        [92; 16],
        old_owner,
    );

    let mut first_issuer = Issuer(1, None);
    let first_credential = create(&mut host, &mut first_issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &first_credential,
        attachment: [6; 16],
        now: 1,
    }))
    .unwrap();
    let second_subscribe = subscribe();
    let mut second_issuer = Issuer(2, None);
    let second_credential = block_on(host.create(
        CreateRequest {
            id: [2; 16],
            origin: origin(),
            expires_at: 100,
            now: 0,
            subscribe: &second_subscribe,
        },
        &mut second_issuer,
    ))
    .unwrap();
    block_on(host.resume(ResumeRequest {
        id: [2; 16],
        origin: &origin(),
        credential: &second_credential,
        attachment: [7; 16],
        now: 1,
    }))
    .unwrap();

    // The status request ID is deliberately shared too. Each session must
    // load its own durable target row and retain its own terminal snapshot.
    let status_request = |fingerprint| {
        Envelope {
            request: Some([44; 16]),
            watch: None,
            message: Message::RequestStatus {
                target: [37; 16],
                fingerprint,
            },
            extensions: BTreeMap::new(),
        }
        .encode(Limits::default().protocol)
        .unwrap()
    };
    let first_status_request = status_request(first_target_fingerprint);
    let second_status_request = status_request(second_target_fingerprint);
    let mut application = UnitApplication::default();
    let first_status = block_on(host.dispatch_frame(
        [6; 16],
        2,
        Frame::Binary(first_status_request.clone()),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the first session reads its recovered target");
    let second_status = block_on(host.dispatch_frame(
        [7; 16],
        2,
        Frame::Binary(second_status_request.clone()),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the second session reads its recovered target");
    let first_result = ResultBody::from_result(
        &unit_result([37; 16], first_target_fingerprint),
        Limits::default().protocol,
    )
    .unwrap();
    let second_result = ResultBody::from_result(
        &semantic_failure_result([37; 16], second_target_fingerprint),
        Limits::default().protocol,
    )
    .unwrap();
    assert!(matches!(
        &first_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Terminal,
            fingerprint: Some(fingerprint),
            result: Some(result),
        } if *target == [37; 16]
            && *fingerprint == first_target_fingerprint
            && result == &first_result
    ));
    assert!(matches!(
        &second_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Terminal,
            fingerprint: Some(fingerprint),
            result: Some(result),
        } if *target == [37; 16]
            && *fingerprint == second_target_fingerprint
            && result == &second_result
    ));

    for (attachment, foreign_fingerprint) in [
        ([6; 16], second_target_fingerprint),
        ([7; 16], first_target_fingerprint),
    ] {
        let mismatch = Envelope {
            request: Some([45; 16]),
            watch: None,
            message: Message::RequestStatus {
                target: [37; 16],
                fingerprint: foreign_fingerprint,
            },
            extensions: BTreeMap::new(),
        }
        .encode(Limits::default().protocol)
        .unwrap();
        let mismatch = block_on(host.dispatch_frame(
            attachment,
            3,
            Frame::Binary(mismatch),
            &mut application,
        ))
        .unwrap()
        .response
        .expect("a foreign fingerprint returns a mismatch diagnostic");
        assert_eq!(mismatch.request, Some([45; 16]));
        assert!(matches!(mismatch.message, Message::Diagnostic { .. }));
    }

    let first_retry = block_on(host.dispatch_frame(
        [6; 16],
        4,
        Frame::Binary(first_status_request),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the first status ID replays in its owning session");
    let second_retry = block_on(host.dispatch_frame(
        [7; 16],
        4,
        Frame::Binary(second_status_request),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the second status ID replays in its owning session");
    assert_eq!(first_retry, first_status);
    assert_eq!(second_retry, second_status);
    assert_eq!(application.calls, 0);

    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_does_not_replay_a_reserved_request_after_host_reconstruction() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [24; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let runtime = open_durable_state(&repository);
    block_on(runtime.reserve_request(
        RequestIdentity {
            session_id: [1; 16],
            request_id: [24; 16],
        },
        fingerprint,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [7; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(
            host.dispatch_frame([7; 16], 2, Frame::Binary(request.clone()), &mut application,)
        )
        .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Accepted)
    );
    assert_eq!(application.calls, 0);

    let duplicate =
        block_on(host.dispatch_frame([7; 16], 2, Frame::Binary(request), &mut application))
            .unwrap();
    assert_eq!(duplicate.outcome, FrameOutcome::Accepted);
    assert!(duplicate.response.is_none());
    assert_eq!(application.calls, 0);

    let status_request = Envelope {
        request: Some([25; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [24; 16],
            fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let status =
        block_on(host.dispatch_frame([7; 16], 3, Frame::Binary(status_request), &mut application))
            .unwrap();
    assert!(matches!(
        status.response.unwrap().message,
        Message::RequestStatusResult {
            target: returned_target,
            state: orna_protocol_v1::RequestState::Reserved,
            fingerprint: Some(returned_fingerprint),
            result: None,
        } if returned_target == [24; 16] && returned_fingerprint == fingerprint
    ));

    let fresh = eval([1; 16], [26; 16], "2");
    assert_eq!(
        block_on(host.dispatch_frame([7; 16], 4, Frame::Binary(fresh), &mut application,))
            .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Accepted)
    );
    assert_eq!(application.calls, 1);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_recovery_does_not_cancel_or_replay_a_running_request() {
    let (root, repository) = durable_repository();
    let target = eval([1; 16], [25; 16], "1");
    let target_fingerprint = request_fingerprint(&target, [1; 16]);
    let runtime = open_durable_state(&repository);
    let target_identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [25; 16],
    };
    block_on(runtime.reserve_request(target_identity, target_fingerprint)).unwrap();
    block_on(runtime.start_request(target_identity, target_fingerprint)).unwrap();
    let old = block_on(runtime.acquire_lease([71; 16])).unwrap();
    block_on(runtime.recover_abandoned(old.owner_id, [72; 16])).unwrap();
    drop(runtime);

    let mut host = durable_host_after_takeover(
        open_durable_state(&repository),
        [72; 16],
        RequestOwner::from(old),
    );
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [8; 16],
        now: 1,
    }))
    .unwrap();
    let cancel = Envelope {
        request: Some([26; 16]),
        watch: None,
        message: Message::Cancel {
            target_kind: TargetKind::Request,
            target: [25; 16],
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame([8; 16], 2, Frame::Binary(cancel), &mut application,))
            .map(|outcome| outcome.outcome),
        Ok(FrameOutcome::Cancelled)
    );
    assert_eq!(application.calls, 0);
    assert!(matches!(
        block_on(host.dispatch_frame([8; 16], 3, Frame::Binary(target), &mut application,))
            .unwrap()
            .outcome,
        FrameOutcome::Accepted
    ));
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_recovers_a_legacy_running_eval_as_uncertain_after_takeover() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [25; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [25; 16],
    };
    let runtime = open_durable_state(&repository);
    block_on(runtime.reserve_request(identity, fingerprint)).unwrap();
    block_on(runtime.start_request(identity, fingerprint)).unwrap();
    let old = block_on(runtime.acquire_lease([81; 16])).unwrap();
    block_on(runtime.recover_abandoned(old.owner_id, [82; 16])).unwrap();
    drop(runtime);

    let mut host = durable_host_after_takeover(
        open_durable_state(&repository),
        [82; 16],
        RequestOwner::from(old),
    );
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [7; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let first =
        block_on(host.dispatch_frame([7; 16], 2, Frame::Binary(request.clone()), &mut application))
            .unwrap();
    assert!(matches!(
        first.response.as_ref().unwrap().message,
        Message::Result {
            status: ResultStatus::RetainedWithoutValue,
            value: None,
            fingerprint: returned_fingerprint,
            diagnostic: None,
        } if returned_fingerprint == fingerprint
    ));
    assert_eq!(application.calls, 0);

    let status = block_on(open_durable_state(&repository).request_status_for_identity(identity))
        .unwrap()
        .unwrap();
    assert_eq!(status.state, orna_runtime_v1::RequestState::Orphaned);
    assert_eq!(status.fingerprint, fingerprint);
    assert_eq!(
        Envelope::decode(
            status.terminal_outcome.as_ref().unwrap().as_bytes(),
            Limits::default().protocol,
        )
        .unwrap(),
        first.response.as_ref().unwrap().clone(),
    );

    let status_request = Envelope {
        request: Some([26; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [25; 16],
            fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let status_outcome =
        block_on(host.dispatch_frame([7; 16], 3, Frame::Binary(status_request), &mut application))
            .unwrap();
    let expected =
        ResultBody::from_result(first.response.as_ref().unwrap(), Limits::default().protocol)
            .unwrap();
    assert!(matches!(
        status_outcome.response.unwrap().message,
        Message::RequestStatusResult {
            target: returned_target,
            state: orna_protocol_v1::RequestState::Orphaned,
            fingerprint: Some(returned_fingerprint),
            result: Some(result),
        } if returned_target == [25; 16]
            && returned_fingerprint == fingerprint
            && result == expected
    ));

    assert_eq!(
        block_on(host.dispatch_frame([7; 16], 4, Frame::Binary(request), &mut application,)),
        Ok(first)
    );
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_recovers_an_owned_running_request_after_takeover() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [76; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [76; 16],
    };
    let runtime = open_durable_state(&repository);
    let old = block_on(runtime.acquire_lease([73; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        old,
        capability,
    ))
    .unwrap();
    block_on(runtime.recover_abandoned(old.owner_id, [74; 16])).unwrap();
    drop(runtime);

    let mut host = durable_host_after_takeover(
        open_durable_state(&repository),
        [74; 16],
        RequestOwner::from(old),
    );
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [9; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let fresh = eval([1; 16], [80; 16], "2");
    let fresh_outcome =
        block_on(host.dispatch_frame([9; 16], 2, Frame::Binary(fresh), &mut application)).unwrap();
    assert_eq!(fresh_outcome.outcome, FrameOutcome::Accepted);
    assert_eq!(application.calls, 1);
    let old_status =
        block_on(open_durable_state(&repository).request_status(identity, fingerprint))
            .unwrap()
            .unwrap();
    assert_eq!(old_status.state, RequestState::Orphaned);
    let recovered =
        block_on(host.dispatch_frame([9; 16], 3, Frame::Binary(request.clone()), &mut application))
            .unwrap();
    assert!(matches!(
        recovered.response.as_ref().unwrap().message,
        Message::Result {
            status: ResultStatus::RetainedWithoutValue,
            ..
        }
    ));
    assert_eq!(application.calls, 1);
    assert_eq!(
        block_on(host.dispatch_frame([9; 16], 4, Frame::Binary(request), &mut application)),
        Ok(recovered)
    );
    assert_eq!(application.calls, 1);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_live_host_does_not_recover_a_request_owned_by_another_runtime() {
    let (root, repository) = durable_repository();
    let active_runtime = open_durable_state(&repository);
    let request = eval([1; 16], [78; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [78; 16],
    };
    let active_owner = block_on(active_runtime.acquire_lease([75; 16])).unwrap();
    let (_, capability) =
        block_on(active_runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(active_runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        active_owner,
        capability,
    ))
    .unwrap();

    // This host has no takeover proof for the active writer. It may observe
    // the request, but must not terminally recover or execute a second copy.
    let mut host = durable_host_with_owner(open_durable_state(&repository), [76; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [10; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();

    let first = block_on(host.dispatch_frame(
        [10; 16],
        2,
        Frame::Binary(request.clone()),
        &mut application,
    ))
    .unwrap();
    assert_eq!(first.outcome, FrameOutcome::Accepted);
    assert!(first.response.is_none());
    assert_eq!(application.calls, 0);

    let status = block_on(
        open_durable_state(&repository).request_status_for_identity(identity),
    )
    .unwrap()
    .unwrap();
    assert_eq!(status.state, RequestState::Running);
    assert_eq!(status.fingerprint, fingerprint);
    assert!(status.terminal_outcome.is_none());
    assert_eq!(
        block_on(
            open_durable_state(&repository).request_owner(identity, fingerprint),
        )
        .unwrap(),
        Some(RequestOwner::from(active_owner))
    );

    // Repeated delivery remains an active observation, never a recovery
    // permission, while the other owner still holds the durable lease.
    let second =
        block_on(host.dispatch_frame([10; 16], 3, Frame::Binary(request), &mut application))
            .unwrap();
    assert_eq!(second.outcome, FrameOutcome::Accepted);
    assert!(second.response.is_none());
    assert_eq!(application.calls, 0);
    drop(host);
    drop(active_runtime);
    remove_test_repository(&root);
}

#[test]
fn durable_live_host_replays_proven_rollback_without_reexecution() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let request = eval([1; 16], [79; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [79; 16],
    };
    let old = block_on(runtime.acquire_lease([73; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        old,
        capability,
    ))
    .unwrap();
    let activation = block_on(runtime.begin_activation()).unwrap();
    let mutation = TableMutation::new([1; 16], "books", vec![1], Some(vec![2])).unwrap();
    assert_eq!(
        block_on(runtime.commit_table_request_activation(
            old,
            identity,
            fingerprint,
            &activation,
            &[mutation],
            [3; 32],
            TerminalOutcome::new(vec![4]).unwrap(),
            &FailAt(FaultPoint::AfterTerminalClaim),
        )),
        Err(RuntimeError::FaultInjected(FaultPoint::AfterTerminalClaim))
    );
    block_on(runtime.recover_abandoned(old.owner_id, [74; 16])).unwrap();
    drop(runtime);

    let mut host = durable_host_after_takeover(
        open_durable_state(&repository),
        [74; 16],
        RequestOwner::from(old),
    );
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [11; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let first =
        block_on(host.dispatch_frame([11; 16], 2, Frame::Binary(request.clone()), &mut application))
            .unwrap();
    assert!(matches!(
        first.response.as_ref().unwrap().message,
        Message::Result {
            status: ResultStatus::Failure,
            value: None,
            fingerprint: returned,
            diagnostic: None,
        } if returned == fingerprint
    ));
    assert_eq!(application.calls, 0);
    let second =
        block_on(host.dispatch_frame([11; 16], 3, Frame::Binary(request), &mut application))
            .unwrap();
    assert_eq!(second, first);
    assert_eq!(application.calls, 0);
    assert_eq!(
        block_on(open_durable_state(&repository).request_recovery_disposition(
            identity,
            fingerprint,
        ))
        .unwrap(),
        Some(orna_runtime_v1::RecoveryDisposition::RollbackProven)
    );
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_replay_rejects_an_uncertain_payload_for_a_proven_rollback() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [77; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [77; 16],
    };
    let runtime = open_durable_state(&repository);
    let old = block_on(runtime.acquire_lease([73; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        old,
        capability,
    ))
    .unwrap();
    let activation = block_on(runtime.begin_activation()).unwrap();
    let mutation = TableMutation::new([1; 16], "books", vec![1], Some(vec![2])).unwrap();
    assert_eq!(
        block_on(runtime.commit_table_request_activation(
            old,
            identity,
            fingerprint,
            &activation,
            &[mutation],
            [3; 32],
            TerminalOutcome::new(vec![4]).unwrap(),
            &FailAt(FaultPoint::AfterTerminalClaim),
        )),
        Err(RuntimeError::FaultInjected(FaultPoint::AfterTerminalClaim))
    );
    let fence = block_on(runtime.recover_abandoned(old.owner_id, [74; 16])).unwrap();
    let mismatched = TerminalOutcome::new(
        Envelope {
            request: Some([77; 16]),
            watch: None,
            message: Message::Result {
                status: ResultStatus::RetainedWithoutValue,
                value: None,
                fingerprint,
                diagnostic: None,
            },
            extensions: BTreeMap::new(),
        }
        .encode(Limits::default().protocol)
        .unwrap(),
    )
    .unwrap();
    block_on(runtime.recover_running_request_with_outcomes(
        identity,
        fingerprint,
        RequestOwner::from(old),
        fence,
        mismatched.clone(),
        mismatched,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), [74; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [9; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame([9; 16], 2, Frame::Binary(request), &mut application)),
        Err(Error::RuntimeUnavailable)
    );
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_replay_rejects_a_rollback_payload_for_external_uncertainty() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [78; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [78; 16],
    };
    let runtime = open_durable_state(&repository);
    let old = block_on(runtime.acquire_lease([75; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        old,
        capability,
    ))
    .unwrap();
    block_on(runtime.record_external_effect(identity, fingerprint, old)).unwrap();
    let fence = block_on(runtime.recover_abandoned(old.owner_id, [76; 16])).unwrap();
    let mismatched = TerminalOutcome::new(
        Envelope {
            request: Some([78; 16]),
            watch: None,
            message: Message::Result {
                status: ResultStatus::Failure,
                value: None,
                fingerprint,
                diagnostic: None,
            },
            extensions: BTreeMap::new(),
        }
        .encode(Limits::default().protocol)
        .unwrap(),
    )
    .unwrap();
    block_on(runtime.recover_running_request_with_outcomes(
        identity,
        fingerprint,
        RequestOwner::from(old),
        fence,
        mismatched.clone(),
        mismatched,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), [76; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [9; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame([9; 16], 2, Frame::Binary(request), &mut application)),
        Err(Error::RuntimeUnavailable)
    );
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
#[allow(clippy::too_many_lines)]
fn durable_runtime_replays_proven_rollback_as_redacted_orphaned_failure() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [79; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [79; 16],
    };
    let runtime = open_durable_state(&repository);
    let old = block_on(runtime.acquire_lease([77; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        old,
        capability,
    ))
    .unwrap();
    let activation = block_on(runtime.begin_activation()).unwrap();
    let mutation = TableMutation::new([1; 16], "books", vec![1], Some(vec![2])).unwrap();
    assert_eq!(
        block_on(runtime.commit_table_request_activation(
            old,
            identity,
            fingerprint,
            &activation,
            &[mutation],
            [3; 32],
            TerminalOutcome::new(vec![4]).unwrap(),
            &FailAt(FaultPoint::AfterTerminalClaim),
        )),
        Err(RuntimeError::FaultInjected(FaultPoint::AfterTerminalClaim))
    );
    block_on(runtime.recover_abandoned(old.owner_id, [78; 16])).unwrap();
    drop(runtime);

    let mut host = durable_host_after_takeover(
        open_durable_state(&repository),
        [78; 16],
        RequestOwner::from(old),
    );
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [11; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let recovered = block_on(host.dispatch_frame(
        [11; 16],
        2,
        Frame::Binary(request.clone()),
        &mut application,
    ))
    .unwrap();
    assert!(matches!(
        recovered.response.as_ref().unwrap().message,
        Message::Result {
            status: ResultStatus::Failure,
            value: None,
            fingerprint: returned,
            diagnostic: None,
        } if returned == fingerprint
    ));
    assert_eq!(application.calls, 0);

    let durable_status =
        block_on(open_durable_state(&repository).request_status_for_identity(identity))
            .unwrap()
            .unwrap();
    assert_eq!(
        durable_status.state,
        orna_runtime_v1::RequestState::Orphaned
    );
    assert_eq!(
        Envelope::decode(
            durable_status.terminal_outcome.as_ref().unwrap().as_bytes(),
            Limits::default().protocol,
        )
        .unwrap(),
        recovered.response.as_ref().unwrap().clone(),
    );

    let status_request = Envelope {
        request: Some([80; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [79; 16],
            fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let status =
        block_on(host.dispatch_frame([11; 16], 3, Frame::Binary(status_request), &mut application))
            .unwrap();
    let expected = ResultBody::from_result(
        recovered.response.as_ref().unwrap(),
        Limits::default().protocol,
    )
    .unwrap();
    assert!(matches!(
        status.response.unwrap().message,
        Message::RequestStatusResult {
            state: orna_protocol_v1::RequestState::Orphaned,
            fingerprint: Some(returned),
            result: Some(result),
            ..
        } if returned == fingerprint && result == expected
    ));
    assert_eq!(
        block_on(host.dispatch_frame([11; 16], 4, Frame::Binary(request), &mut application)),
        Ok(recovered)
    );
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_reports_a_current_owner_as_active_without_reexecution() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [77; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [77; 16],
    };
    let runtime = open_durable_state(&repository);
    let owner = block_on(runtime.acquire_lease([75; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        owner,
        capability,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), [76; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [10; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let outcome =
        block_on(host.dispatch_frame([10; 16], 2, Frame::Binary(request), &mut application))
            .unwrap();
    assert_eq!(outcome.outcome, FrameOutcome::Accepted);
    assert!(outcome.response.is_none());
    assert_eq!(application.calls, 0);
    let status = block_on(open_durable_state(&repository).request_status(identity, fingerprint))
        .unwrap()
        .unwrap();
    assert_eq!(status.state, orna_runtime_v1::RequestState::Running);
    assert!(status.terminal_outcome.is_none());
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_dispatch_rejects_a_fenced_owner_before_cancellation_callback() {
    let (root, repository) = durable_repository();
    let mut host = durable_host_with_owner(open_durable_state(&repository), [66; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [12; 16],
        now: 1,
    }))
    .unwrap();

    let mut application = UnitApplication::default();
    block_on(host.dispatch_frame(
        [12; 16],
        2,
        Frame::Binary(eval([1; 16], [67; 16], "warm")),
        &mut application,
    ))
    .unwrap();
    assert_eq!(application.calls, 1);

    let target = RequestIdentity {
        session_id: [1; 16],
        request_id: [65; 16],
    };
    let target_fingerprint = request_fingerprint(&eval([1; 16], [65; 16], "target"), [1; 16]);
    let target_runtime = open_durable_state(&repository);
    let old = block_on(target_runtime.acquire_lease([66; 16])).unwrap();
    let (_, capability) =
        block_on(target_runtime.reserve_request_with_admission(target, target_fingerprint))
            .unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(target_runtime.start_request_with_owner_and_admission(
        target,
        target_fingerprint,
        old,
        capability,
    ))
    .unwrap();
    block_on(target_runtime.recover_abandoned(old.owner_id, [68; 16])).unwrap();
    drop(target_runtime);

    let cancellation = cancel_request([64; 16], [65; 16]);
    assert_eq!(
        block_on(host.dispatch_frame([12; 16], 3, Frame::Binary(cancellation), &mut application,)),
        Err(Error::RuntimeUnavailable)
    );
    assert_eq!(application.calls, 1);

    let status =
        block_on(open_durable_state(&repository).request_status(target, target_fingerprint))
            .unwrap()
            .unwrap();
    assert_eq!(status.state, orna_runtime_v1::RequestState::Running);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_rejects_a_stale_terminal_owner_transition() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [78; 16],
    };
    let fingerprint = [78; 32];
    let old = block_on(runtime.acquire_lease([77; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        old,
        capability,
    ))
    .unwrap();
    block_on(runtime.recover_abandoned(old.owner_id, [78; 16])).unwrap();
    assert_eq!(
        block_on(runtime.complete_request_with_owner(
            identity,
            fingerprint,
            old,
            TerminalOutcome::new(Vec::new()).unwrap(),
        )),
        Err(RuntimeError::OwnerLost)
    );
    assert!(matches!(
        block_on(runtime.request_status(identity, fingerprint)).unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Running
    ));
    drop(runtime);
    remove_test_repository(&root);
}

#[test]
fn durable_runtime_rejects_a_retained_response_with_the_wrong_message_shape() {
    let (root, repository) = durable_repository();
    let request = eval([1; 16], [27; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let retained = Envelope {
        request: Some([27; 16]),
        watch: None,
        message: Message::RequestStatusResult {
            target: [28; 16],
            state: orna_protocol_v1::RequestState::Unknown,
            fingerprint: None,
            result: None,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let runtime = open_durable_state(&repository);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [27; 16],
    };
    block_on(runtime.reserve_request(identity, fingerprint)).unwrap();
    block_on(runtime.start_request(identity, fingerprint)).unwrap();
    block_on(runtime.complete_request(
        identity,
        fingerprint,
        TerminalOutcome::new(retained).unwrap(),
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host(open_durable_state(&repository));
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [9; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    assert_eq!(
        block_on(host.dispatch_frame([9; 16], 2, Frame::Binary(request), &mut application,)),
        Err(Error::RuntimeUnavailable)
    );
    assert_eq!(application.calls, 0);
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn completion_race_rejects_a_terminal_winner_with_mismatched_request_bytes() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let owner = [91; 16];
    let mut host = durable_host_with_owner(open_durable_state(&repository), owner);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [9; 16],
        now: 1,
    }))
    .unwrap();

    let request = eval([1; 16], [92; 16], "1");
    let fingerprint = request_fingerprint(&request, [1; 16]);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [92; 16],
    };
    let mut application = CompetingTerminalApplication {
        repository: repository.clone(),
        owner,
        calls: 0,
    };
    assert_eq!(
        block_on(host.dispatch_frame([9; 16], 2, Frame::Binary(request.clone()), &mut application)),
        Err(Error::RuntimeUnavailable)
    );
    assert_eq!(application.calls, 1);

    let status = block_on(runtime.request_status(identity, fingerprint))
        .unwrap()
        .unwrap();
    assert_eq!(status.state, orna_runtime_v1::RequestState::Completed);
    assert_eq!(
        Envelope::decode(
            status.terminal_outcome.unwrap().as_bytes(),
            Limits::default().protocol,
        )
        .unwrap()
        .request,
        Some([99; 16])
    );
    assert_eq!(
        block_on(runtime.committed_table_row("books", &[8])),
        Ok(None)
    );
    assert_eq!(
        block_on(host.dispatch_frame([9; 16], 3, Frame::Binary(request), &mut application)),
        Err(Error::RuntimeUnavailable)
    );
    assert_eq!(application.calls, 1);
    drop(host);
    drop(runtime);
    remove_test_repository(&root);
}

#[test]
fn cancelling_a_terminal_request_is_idempotent_and_preserves_its_result() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let target = eval([1; 16], [20; 16], "1");
    let target_outcome =
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(target.clone()), &mut application))
            .unwrap();
    let cancel = Envelope {
        request: Some([22; 16]),
        watch: None,
        message: Message::Cancel {
            target_kind: TargetKind::Request,
            target: [20; 16],
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(cancel), &mut application,))
            .unwrap()
            .outcome,
        FrameOutcome::Cancelled
    );
    assert_eq!(application.calls, 1);
    assert_eq!(
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(target), &mut application,)),
        Ok(target_outcome)
    );
    assert_eq!(application.calls, 1);
}

#[test]
fn dispatches_request_status_and_rejects_unsupported_client_operations() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    host.reserve_request([1; 16], [8; 16]).unwrap();
    let status = Envelope {
        request: Some([9; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [8; 16],
            fingerprint: [0; 32],
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    assert_eq!(
        block_on(host.handle_frame([5; 16], 2, Frame::Binary(status))),
        Ok(FrameOutcome::Accepted)
    );
    assert_eq!(
        block_on(host.handle_frame([5; 16], 2, Frame::Binary(subscribe()))),
        Err(Error::UnsupportedOperation)
    );
}

#[test]
fn unsubscribe_of_an_absent_watch_is_an_idempotent_unit_success() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [5; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    let outcome =
        block_on(host.dispatch_frame([5; 16], 2, Frame::Binary(unsubscribe()), &mut application))
            .unwrap();
    assert_eq!(outcome.outcome, FrameOutcome::Accepted);
    assert!(matches!(
        outcome.response.unwrap().message,
        Message::Result {
            status: ResultStatus::Success,
            value: Some(_),
            ..
        }
    ));
    assert_eq!(application.calls, 0);
}

#[test]
fn request_retention_cannot_be_shorter_than_the_reconnect_lease() {
    let mut limits = TransportLimits::default();
    limits.request_retention_ms -= 1;
    assert!(matches!(
        LiveTransport::new(host(), limits),
        Err(Error::Limit)
    ));
}

#[test]
fn deletion_failure_closes_fail_closed_without_sensitive_diagnostics() {
    let mut host = host();
    let mut issuer = Issuer(0x5a, None);
    let credential = create(&mut host, &mut issuer);
    let rendered = format!("{credential:?} {}", Error::DeletionFailed);
    assert!(!rendered.contains("5a"));
    assert!(!rendered.contains("app.example"));
    assert_eq!(
        block_on(host.delete(
            DeleteRequest {
                id: [1; 16],
                origin: &origin(),
                credential: &credential,
                now: 1,
            },
            &mut Delete(false),
        )),
        Err(Error::DeletionFailed)
    );
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [5; 16],
            now: 1
        })),
        Err(Error::Closed)
    );
}

#[test]
fn delete_cancels_durable_session_work_before_returning_success() {
    let (root, repository) = durable_repository();
    // The reference requires DELETE to cancel session-owned work, but leaves
    // the same-ID running-status behavior across sessions unspecified. Prove
    // the neighbor's active row survives closure and can still complete.
    let first_target_request = eval_with_context([1; 16], [91; 16], [2; 16], None);
    let first_fingerprint = request_fingerprint(&first_target_request, [1; 16]);
    let second_target_request = eval_with_context([2; 16], [91; 16], [2; 16], None);
    let second_fingerprint = request_fingerprint(&second_target_request, [2; 16]);
    assert_ne!(first_fingerprint, second_fingerprint);

    let runtime = open_durable_state(&repository);
    let owner = [93; 16];
    let lease = block_on(runtime.acquire_lease(owner)).unwrap();
    let second_identity = RequestIdentity {
        session_id: [2; 16],
        request_id: [91; 16],
    };
    let second_result = unit_result([91; 16], second_fingerprint);
    let (_, second_capability) =
        block_on(runtime.reserve_request_with_admission(second_identity, second_fingerprint))
            .unwrap();
    let second_capability = second_capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        second_identity,
        second_fingerprint,
        lease,
        second_capability,
    ))
    .unwrap();

    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [91; 16],
    };
    let fingerprint = first_fingerprint;
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        lease,
        capability,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), owner);
    let mut first_issuer = Issuer(1, None);
    let credential = create(&mut host, &mut first_issuer);
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin(),
        credential: &credential,
        attachment: [6; 16],
        now: 1,
    }))
    .unwrap();
    let second_subscribe = subscribe();
    let mut second_issuer = Issuer(2, None);
    let second_credential = block_on(host.create(
        CreateRequest {
            id: [2; 16],
            origin: origin(),
            expires_at: 100,
            now: 0,
            subscribe: &second_subscribe,
        },
        &mut second_issuer,
    ))
    .unwrap();
    block_on(host.resume(ResumeRequest {
        id: [2; 16],
        origin: &origin(),
        credential: &second_credential,
        attachment: [7; 16],
        now: 1,
    }))
    .unwrap();

    let origin = origin();
    let mut deletion = RecordingDelete::default();
    let mut children = RecordingChildren::default();
    let mut application = UnitApplication::default();
    let first_status_request = Envelope {
        request: Some([44; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [91; 16],
            fingerprint: first_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let first_status = block_on(host.dispatch_frame(
        [6; 16],
        2,
        Frame::Binary(first_status_request.clone()),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the owning session can read its running request");
    assert!(matches!(
        first_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Running,
            fingerprint: Some(returned),
            result: None,
        } if target == [91; 16] && returned == first_fingerprint
    ));

    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(deletion.calls, 1);
    assert_eq!(children.calls, 1);
    assert_eq!(children.requests, vec![identity]);

    let second_running_status_request = Envelope {
        request: Some([44; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [91; 16],
            fingerprint: second_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let second_running_status = block_on(host.dispatch_frame(
        [7; 16],
        2,
        Frame::Binary(second_running_status_request.clone()),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the other session keeps its same-ID request status after deletion");
    assert!(matches!(
        &second_running_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Running,
            fingerprint: Some(returned),
            result: None,
        } if *target == [91; 16] && *returned == second_fingerprint
    ));
    assert!(matches!(
        block_on(open_durable_state(&repository).request_status(second_identity, second_fingerprint))
            .unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Running
    ));

    block_on(open_durable_state(&repository).complete_request_with_owner(
        second_identity,
        second_fingerprint,
        lease,
        TerminalOutcome::new(second_result.encode(Limits::default().protocol).unwrap()).unwrap(),
    ))
    .unwrap();
    let second_status_request = Envelope {
        request: Some([45; 16]),
        watch: None,
        message: Message::RequestStatus {
            target: [91; 16],
            fingerprint: second_fingerprint,
        },
        extensions: BTreeMap::new(),
    }
    .encode(Limits::default().protocol)
    .unwrap();
    let second_status = block_on(host.dispatch_frame(
        [7; 16],
        3,
        Frame::Binary(second_status_request.clone()),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the surviving session can recover its completion after deletion");
    let expected_result =
        ResultBody::from_result(&second_result, Limits::default().protocol).unwrap();
    assert!(matches!(
        &second_status.message,
        Message::RequestStatusResult {
            target,
            state: orna_protocol_v1::RequestState::Terminal,
            fingerprint: Some(returned),
            result: Some(result),
        } if *target == [91; 16]
            && *returned == second_fingerprint
            && result == &expected_result
    ));
    assert!(matches!(
        block_on(host.dispatch_frame(
            [6; 16],
            2,
            Frame::Binary(first_status_request),
            &mut application,
        )),
        Err(Error::Closed)
    ), "a deleted session cannot continue its status stream");

    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(deletion.calls, 1);
    assert_eq!(children.calls, 1);
    assert!(matches!(
        block_on(open_durable_state(&repository).request_status(identity, fingerprint)).unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Cancelled
    ));
    assert!(matches!(
        block_on(open_durable_state(&repository).request_status(second_identity, second_fingerprint))
            .unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Completed
    ));

    let second_status_retry = block_on(host.dispatch_frame(
        [7; 16],
        4,
        Frame::Binary(second_status_request),
        &mut application,
    ))
    .unwrap()
    .response
    .expect("the surviving session replays its own status snapshot");
    assert_eq!(second_status_retry, second_status);
    assert_eq!(application.calls, 0);
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin,
            credential: &credential,
            attachment: [94; 16],
            now: 1,
        })),
        Err(Error::Closed)
    );
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn delete_enumerates_reserved_durable_work_before_joining_children() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [94; 16],
    };
    let fingerprint = [95; 32];
    block_on(runtime.reserve_request(identity, fingerprint)).unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), [96; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    let origin = origin();
    let mut deletion = RecordingDelete::default();
    let mut children = RecordingChildren::default();
    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(children.requests, vec![identity]);
    assert!(matches!(
        block_on(open_durable_state(&repository).request_status(identity, fingerprint)).unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Cancelled
    ));
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn failed_child_join_after_durable_cancellation_never_reports_delete_success() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let reserved = RequestIdentity {
        session_id: [1; 16],
        request_id: [96; 16],
    };
    let running = RequestIdentity {
        session_id: [1; 16],
        request_id: [97; 16],
    };
    let reserved_fingerprint = [98; 32];
    let running_fingerprint = [99; 32];
    let owner = [100; 16];
    let lease = block_on(runtime.acquire_lease(owner)).unwrap();
    block_on(runtime.reserve_request(reserved, reserved_fingerprint)).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(running, running_fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        running,
        running_fingerprint,
        lease,
        capability,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), owner);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    let origin = origin();
    let mut deletion = RecordingDelete::default();
    let mut children = RecordingChildren {
        fail: true,
        ..RecordingChildren::default()
    };
    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        400
    );
    assert_eq!(deletion.calls, 0);
    assert_eq!(children.calls, 1);
    assert_eq!(children.requests, vec![reserved, running]);
    // A failed join leaves the session fenced. Retrying the same authenticated
    // DELETE must never turn the prior failed cleanup into an idempotent 204.
    assert_ne!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(deletion.calls, 0);
    assert_eq!(children.calls, 1);
    for (identity, fingerprint) in [
        (reserved, reserved_fingerprint),
        (running, running_fingerprint),
    ] {
        assert!(matches!(
            block_on(open_durable_state(&repository).request_status(identity, fingerprint))
                .unwrap(),
            Some(status) if status.state == orna_runtime_v1::RequestState::Cancelled
        ));
    }
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin,
            credential: &credential,
            attachment: [101; 16],
            now: 1,
        })),
        Err(Error::Closed)
    );
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn durable_admission_during_child_drain_is_rejected_by_the_close_fence() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let initial = RequestIdentity {
        session_id: [1; 16],
        request_id: [102; 16],
    };
    let late = RequestIdentity {
        session_id: [1; 16],
        request_id: [103; 16],
    };
    let initial_fingerprint = [104; 32];
    let late_fingerprint = [105; 32];
    block_on(runtime.reserve_request(initial, initial_fingerprint)).unwrap();

    let mut host = durable_host_with_owner(open_durable_state(&repository), [106; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    let origin = origin();
    let mut deletion = RecordingDelete::default();
    let mut children = LateAdmissionChildren {
        runtime: &runtime,
        identity: late,
        fingerprint: late_fingerprint,
        calls: 0,
    };
    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(children.calls, 1);
    assert_eq!(deletion.calls, 1);
    assert!(matches!(
        block_on(runtime.request_status(initial, initial_fingerprint)).unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Cancelled
    ));
    assert_eq!(
        block_on(runtime.request_status(late, late_fingerprint)).unwrap(),
        None
    );
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin,
            credential: &credential,
            attachment: [107; 16],
            now: 1,
        })),
        Err(Error::Closed)
    );
    drop(host);
    drop(runtime);
    remove_test_repository(&root);
}

#[test]
fn application_admission_requires_an_explicit_delete_join_boundary() {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    let origin = origin();
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin,
        credential: &credential,
        attachment: [97; 16],
        now: 1,
    }))
    .unwrap();
    let mut application = UnitApplication::default();
    block_on(host.dispatch_frame(
        [97; 16],
        2,
        Frame::Binary(eval([1; 16], [98; 16], "1")),
        &mut application,
    ))
    .unwrap();

    let mut deletion = RecordingDelete::default();
    assert_eq!(
        block_on(host.http_delete(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 2,
            },
            &mut deletion,
        ))
        .status,
        400
    );
    assert_eq!(deletion.calls, 0);

    let mut children = RecordingChildren::default();
    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 2,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(children.calls, 1);
    assert!(children.requests.is_empty());
}

#[test]
fn failed_durable_drain_never_reports_delete_success() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [95; 16],
    };
    let fingerprint = [96; 32];
    let lease = block_on(runtime.acquire_lease([97; 16])).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        lease,
        capability,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), [98; 16]);
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    let origin = origin();
    let mut deletion = RecordingDelete::default();
    let mut children = RecordingChildren::default();
    assert_eq!(
        block_on(host.http_delete_with_children(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
            &mut children,
        ))
        .status,
        503
    );
    assert_eq!(deletion.calls, 0);
    assert_eq!(children.calls, 0);
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin,
            credential: &credential,
            attachment: [99; 16],
            now: 1,
        })),
        Err(Error::Closed)
    );
    assert!(matches!(
        block_on(open_durable_state(&repository).request_status(identity, fingerprint)).unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Running
    ));
    drop(host);
    remove_test_repository(&root);
}

#[test]
fn expired_delete_cannot_cancel_durable_session_work() {
    let (root, repository) = durable_repository();
    let runtime = open_durable_state(&repository);
    let identity = RequestIdentity {
        session_id: [1; 16],
        request_id: [100; 16],
    };
    let fingerprint = [101; 32];
    let owner = [102; 16];
    let lease = block_on(runtime.acquire_lease(owner)).unwrap();
    let (_, capability) =
        block_on(runtime.reserve_request_with_admission(identity, fingerprint)).unwrap();
    let capability = capability.expect("fresh owner-bound capability");
    block_on(runtime.start_request_with_owner_and_admission(
        identity,
        fingerprint,
        lease,
        capability,
    ))
    .unwrap();
    drop(runtime);

    let mut host = durable_host_with_owner(open_durable_state(&repository), owner);
    let mut issuer = Issuer(1, None);
    let credential = block_on(host.create(
        CreateRequest {
            id: [1; 16],
            origin: origin(),
            expires_at: 1,
            now: 0,
            subscribe: &subscribe(),
        },
        &mut issuer,
    ))
    .unwrap();
    let origin = origin();
    let mut deletion = RecordingDelete::default();
    assert_eq!(
        block_on(host.http_delete(
            DeleteRequest {
                id: [1; 16],
                origin: &origin,
                credential: &credential,
                now: 1,
            },
            &mut deletion,
        ))
        .status,
        410
    );
    assert_eq!(deletion.calls, 0);
    assert!(matches!(
        block_on(open_durable_state(&repository).request_status(identity, fingerprint)).unwrap(),
        Some(status) if status.state == orna_runtime_v1::RequestState::Running
    ));
    drop(host);
    remove_test_repository(&root);
}

fn wire(method: &str, path: &str, body: &str) -> WireRequest {
    WireRequest {
        method: method.into(),
        path: path.into(),
        headers: vec![
            ("origin".into(), "https://app.example".into()),
            ("host".into(), "app.example".into()),
            ("content-type".into(), "application/json".into()),
        ],
        body: body.as_bytes().to_vec(),
    }
}
fn uuid(value: u8) -> String {
    let raw = format!("{value:02x}").repeat(16);
    format!(
        "{}-{}-{}-{}-{}",
        &raw[..8],
        &raw[8..12],
        &raw[12..16],
        &raw[16..20],
        &raw[20..]
    )
}
fn token(response: &orna_live_v1::WireResponse) -> String {
    let body = String::from_utf8(response.body.clone()).unwrap();
    body.split("\"resume_token\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .into()
}
fn websocket_upgrade(session: u8, credential: &str) -> WireRequest {
    let mut request = wire("GET", &format!("/orna/live/{}", uuid(session)), "");
    request.headers.extend([
        ("connection".into(), "Upgrade".into()),
        ("upgrade".into(), "websocket".into()),
        ("sec-websocket-version".into(), "13".into()),
        (
            "sec-websocket-key".into(),
            "dGhlIHNhbXBsZSBub25jZQ==".into(),
        ),
        ("sec-websocket-protocol".into(), SUBPROTOCOL.into()),
        ("cookie".into(), format!("orna_session={credential}")),
    ]);
    request
}
fn masked(fin: bool, opcode: u8, body: &[u8]) -> Vec<u8> {
    assert!(body.len() < 126);
    let key = [1, 2, 3, 4];
    let length = u8::try_from(body.len()).expect("test frame is short");
    let mut frame = vec![(if fin { 128 } else { 0 }) | opcode, 128 | length];
    frame.extend(key);
    frame.extend(
        body.iter()
            .enumerate()
            .map(|(index, byte)| byte ^ key[index % 4]),
    );
    frame
}

fn masked_binary_payload(body: &[u8]) -> Vec<u8> {
    if body.len() < 126 {
        masked(true, 2, body)
    } else {
        masked_with_length_code(
            true,
            2,
            body,
            126,
            u64::try_from(body.len()).expect("test payload length fits u64"),
        )
    }
}

fn unmasked(fin: bool, opcode: u8, body: &[u8]) -> Vec<u8> {
    assert!(body.len() < 126);
    let length = u8::try_from(body.len()).expect("test frame is short");
    let mut frame = vec![(if fin { 128 } else { 0 }) | opcode, length];
    frame.extend_from_slice(body);
    frame
}

fn masked_with_length_code(
    fin: bool,
    opcode: u8,
    body: &[u8],
    length_code: u8,
    encoded_length: u64,
) -> Vec<u8> {
    let key = [1, 2, 3, 4];
    let mut frame = vec![(if fin { 128 } else { 0 }) | opcode, 128 | length_code];
    match length_code {
        126 => frame
            .extend_from_slice(&(u16::try_from(encoded_length).expect("u16 length")).to_be_bytes()),
        127 => frame.extend_from_slice(&encoded_length.to_be_bytes()),
        _ => unreachable!("test helper only encodes extended lengths"),
    }
    frame.extend(key);
    frame.extend(
        body.iter()
            .enumerate()
            .map(|(index, byte)| byte ^ key[index % 4]),
    );
    frame
}

#[test]
#[allow(clippy::too_many_lines)]
fn live_http_routes_are_exact_origin_checked_and_rotate_scoped_tokens() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let rejected = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            r#"{"database":"bad","protocol":"orna.present.v1"}"#,
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(rejected.status, 400);
    let mut missing_type = wire(
        "POST",
        "/orna/session",
        &format!(
            r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
            uuid(2)
        ),
    );
    missing_type
        .headers
        .retain(|(name, _)| name != "content-type");
    assert_eq!(
        block_on(transport.handle(missing_type, 0, &mut authority, &mut issuer, &mut deletion,))
            .status,
        400
    );
    let rejected = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2),
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(rejected.status, 400);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(created.status, 201, "{created:?}");
    assert!(
        String::from_utf8(created.body.clone())
            .unwrap()
            .contains("\"websocket_path\":\"/orna/live/01010101-0101-0101-0101-010101010101\"")
    );
    assert!(created.headers[1].1.contains(
        "Path=/orna/live/01010101-0101-0101-0101-010101010101; HttpOnly; SameSite=Strict; Secure"
    ));
    let first = token(&created);
    let resumed = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session/01010101-0101-0101-0101-010101010101/resume",
            &format!(r#"{{"resume_token":"{first}","protocol":"orna.present.v1"}}"#),
        ),
        1,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(resumed.status, 200);
    let second = token(&resumed);
    assert_ne!(first, second);
    let mut upgrade = wire("GET", "/orna/live/01010101-0101-0101-0101-010101010101", "");
    upgrade.headers.extend([
        ("connection".into(), "Upgrade".into()),
        ("upgrade".into(), "websocket".into()),
        ("sec-websocket-version".into(), "13".into()),
        (
            "sec-websocket-key".into(),
            "dGhlIHNhbXBsZSBub25jZQ==".into(),
        ),
        ("sec-websocket-protocol".into(), SUBPROTOCOL.into()),
        ("cookie".into(), format!("orna_session={second}")),
    ]);
    assert_eq!(block_on(transport.upgrade(upgrade, [5; 16], 2)).status, 101);
    let replay = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session/01010101-0101-0101-0101-010101010101/resume",
            &format!(r#"{{"resume_token":"{first}","protocol":"orna.present.v1"}}"#),
        ),
        2,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(replay.status, 410);
    let mut deleted = wire(
        "DELETE",
        "/orna/session/01010101-0101-0101-0101-010101010101",
        "",
    );
    deleted
        .headers
        .push(("authorization".into(), format!("Bearer {second}")));
    let mut children = RecordingChildren::default();
    assert_eq!(
        block_on(transport.handle_with_children(
            deleted,
            2,
            &mut authority,
            &mut issuer,
            &mut deletion,
            &mut children,
        ))
        .status,
        204
    );
    assert_eq!(transport.take_retired_attachments(), vec![[5; 16]]);
}

#[test]
fn malformed_create_json_is_rejected_before_session_admission() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut authority = CountingAuthority {
        calls: 0,
        times: Vec::new(),
    };
    let mut issuer = Issuer(1, None);
    let mut deletion = Delete(true);

    let response = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1",}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));

    assert_eq!(response.status, 400);
    assert_eq!(authority.calls, 0);
    assert_eq!(issuer.1, None);
}

#[test]
fn malformed_resume_json_is_rejected_before_attachment_replacement() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut authority = Authority;
    let mut issuer = Issuer(1, None);
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(created.status, 201);
    let original = token(&created);

    let response = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session/01010101-0101-0101-0101-010101010101/resume",
            &format!(r#"{{"resume_token":"{original}","protocol":"orna.present.v1",}}"#),
        ),
        1,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));

    assert_eq!(response.status, 400);
    assert_eq!(issuer.1, Some([1; 32]));

    let resumed = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session/01010101-0101-0101-0101-010101010101/resume",
            &format!(r#"{{"resume_token":"{original}","protocol":"orna.present.v1"}}"#),
        ),
        2,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    assert_eq!(resumed.status, 200);
    assert_ne!(token(&resumed), original);
}

#[test]
#[allow(clippy::too_many_lines)]
fn websocket_upgrade_fragmentation_and_controls_are_checked_and_forwarded() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let cookie = token(&created);
    let mut upgrade = wire("GET", "/orna/live/01010101-0101-0101-0101-010101010101", "");
    upgrade.headers.extend([
        ("connection".into(), "Upgrade".into()),
        ("upgrade".into(), "websocket".into()),
        ("sec-websocket-version".into(), "13".into()),
        (
            "sec-websocket-key".into(),
            "dGhlIHNhbXBsZSBub25jZQ==".into(),
        ),
        (
            "sec-websocket-protocol".into(),
            format!("other, {SUBPROTOCOL}"),
        ),
        ("cookie".into(), format!("orna_session={cookie}")),
    ]);
    let upgraded = block_on(transport.upgrade(upgrade.clone(), [5; 16], 1));
    assert_eq!(upgraded.status, 101);
    assert!(
        upgraded
            .headers
            .iter()
            .any(|(name, value)| name == "sec-websocket-accept"
                && value == "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=")
    );
    let mut no_protocol = upgrade;
    no_protocol
        .headers
        .retain(|(name, _)| name != "sec-websocket-protocol");
    assert_eq!(
        block_on(transport.upgrade(no_protocol, [6; 16], 1)).status,
        400
    );
    let message = resync();
    let split = message.len() / 2;
    let mut socket = WebSocketState::new([5; 16]);
    assert!(
        block_on(transport.receive(&mut socket, 2, &masked(false, 2, &message[..split])))
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        block_on(transport.receive(&mut socket, 2, &masked(true, 0, &message[split..]))),
        Ok(outputs)
            if matches!(
                outputs.as_slice(),
                [WebSocketOutput::Binary {
                    outcome: FrameOutcome::Accepted,
                    ..
                }]
            )
    ));
    let pong = block_on(transport.receive(&mut socket, 2, &masked(true, 9, b"p"))).unwrap();
    assert_eq!(pong, vec![WebSocketOutput::Pong(b"p".to_vec())]);
    assert_eq!(
        encode_websocket_output(&pong[0], TransportLimits::default()).unwrap(),
        Some(vec![0x8a, 1, b'p'])
    );
    let close = block_on(transport.receive(
        &mut socket,
        2,
        &[masked(true, 8, b""), masked(true, 9, b"ignored ping")].concat(),
    ))
    .unwrap();
    assert_eq!(
        close,
        vec![WebSocketOutput::Close {
            code: None,
            reason: Vec::new(),
        }]
    );
    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &masked(true, 2, &message))),
        Err(Error::Closed)
    );
    assert_eq!(
        block_on(transport.receive(
            &mut WebSocketState::new([5; 16]),
            2,
            &masked(true, 1, b"text"),
        )),
        Ok(vec![WebSocketOutput::Close {
            code: Some(1003),
            reason: Vec::new(),
        }])
    );
}

#[test]
fn websocket_output_encoder_emits_minimal_unmasked_frames() {
    for (size, header) in [(0, vec![0x82, 0]), (125, vec![0x82, 125])] {
        let output = WebSocketOutput::Binary {
            outcome: FrameOutcome::Accepted,
            payload: vec![7; size],
        };
        let encoded = encode_websocket_output(&output, TransportLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!(&encoded[..header.len()], header.as_slice());
        assert_eq!(encoded.len(), header.len() + size);
    }
    let cases = [
        (126, vec![0x82, 126, 0, 126]),
        (u16::MAX as usize, vec![0x82, 126, 255, 255]),
        (
            u16::MAX as usize + 1,
            vec![0x82, 127, 0, 0, 0, 0, 0, 1, 0, 0],
        ),
    ];
    for (size, header) in cases {
        let output = WebSocketOutput::Binary {
            outcome: FrameOutcome::Accepted,
            payload: vec![7; size],
        };
        let encoded = encode_websocket_output(&output, TransportLimits::default())
            .unwrap()
            .unwrap();
        assert_eq!(&encoded[..header.len()], header.as_slice());
        assert_eq!(encoded.len(), header.len() + size);
    }
    assert_eq!(
        encode_websocket_output(
            &WebSocketOutput::Pong(vec![1, 2]),
            TransportLimits::default()
        )
        .unwrap(),
        Some(vec![0x8a, 2, 1, 2])
    );
    assert_eq!(
        encode_websocket_output(
            &WebSocketOutput::Close {
                code: None,
                reason: Vec::new(),
            },
            TransportLimits::default()
        )
        .unwrap(),
        Some(vec![0x88, 0])
    );
    assert_eq!(
        encode_websocket_output(
            &WebSocketOutput::Close {
                code: Some(1002),
                reason: Vec::new(),
            },
            TransportLimits::default()
        )
        .unwrap(),
        Some(vec![0x88, 2, 0x03, 0xea])
    );
    assert_eq!(
        encode_websocket_output(
            &WebSocketOutput::Accepted(FrameOutcome::Accepted),
            TransportLimits::default()
        )
        .unwrap(),
        None
    );
}

#[test]
fn websocket_output_encoder_rejects_oversized_payloads_before_encoding() {
    let limits = TransportLimits {
        max_frame_bytes: 2,
        max_outgoing_bytes: 2,
        ..TransportLimits::default()
    };
    assert_eq!(
        encode_websocket_output(
            &WebSocketOutput::Binary {
                outcome: FrameOutcome::Accepted,
                payload: vec![7; 3],
            },
            limits
        ),
        Err(orna_live_v1::WebSocketEncodeError::Limit)
    );
    let control_limits = TransportLimits {
        max_frame_bytes: 256,
        max_outgoing_bytes: 256,
        ..TransportLimits::default()
    };
    assert_eq!(
        encode_websocket_output(&WebSocketOutput::Pong(vec![7; 126]), control_limits),
        Err(orna_live_v1::WebSocketEncodeError::Limit)
    );
}

fn attached_transport_for_close() -> (LiveTransport, WebSocketState) {
    let mut host = host();
    let mut issuer = Issuer(1, None);
    let credential = create(&mut host, &mut issuer);
    assert_eq!(
        block_on(host.resume(ResumeRequest {
            id: [1; 16],
            origin: &origin(),
            credential: &credential,
            attachment: [5; 16],
            now: 1,
        }))
        .unwrap(),
        orna_security_v1::AttachOutcome::Attached
    );
    (
        LiveTransport::new(host, TransportLimits::default()).unwrap(),
        WebSocketState::new([5; 16]),
    )
}

fn close_output(
    transport: &mut LiveTransport,
    socket: &mut WebSocketState,
    frame: &[u8],
) -> WebSocketOutput {
    let outputs = block_on(transport.receive(socket, 2, frame)).unwrap();
    assert_eq!(outputs.len(), 1);
    outputs.into_iter().next().unwrap()
}

fn prepared_close_output(
    transport: &mut LiveTransport,
    socket: &mut WebSocketState,
    frame: &[u8],
) -> WebSocketOutput {
    match block_on(transport.prepare_websocket_application(socket, 2, frame)).unwrap() {
        orna_live_v1::WebSocketApplicationPreparation::Output(output) => output,
        orna_live_v1::WebSocketApplicationPreparation::Pending
        | orna_live_v1::WebSocketApplicationPreparation::Work(_) => {
            panic!("expected prepared Close output")
        }
    }
}

fn assert_close_output(output: &WebSocketOutput, code: Option<u16>, reason: &[u8]) {
    match output {
        WebSocketOutput::Close {
            code: actual_code,
            reason: actual_reason,
        } => {
            assert_eq!(*actual_code, code);
            assert_eq!(actual_reason, reason);
        }
        other => panic!("expected Close output, got {other:?}"),
    }
}

fn encode_close(output: &WebSocketOutput) -> Vec<u8> {
    encode_websocket_output(output, TransportLimits::default())
        .unwrap()
        .expect("Close output must encode")
}

#[test]
fn websocket_peer_close_echoes_valid_code_and_reason_on_receive_and_prepare() {
    let mut payload = vec![0x03, 0xe8];
    payload.extend_from_slice(b"normal");

    let (mut transport, mut socket) = attached_transport_for_close();
    let receive = close_output(&mut transport, &mut socket, &masked(true, 8, &payload));
    assert_close_output(&receive, Some(1000), b"normal");
    assert_eq!(
        encode_close(&receive),
        [0x88, 8, 0x03, 0xe8, b'n', b'o', b'r', b'm', b'a', b'l']
    );

    let (mut transport, mut socket) = attached_transport_for_close();
    let prepared =
        prepared_close_output(&mut transport, &mut socket, &masked(true, 8, &payload));
    assert_close_output(&prepared, Some(1000), b"normal");
    assert_eq!(
        encode_close(&prepared),
        [0x88, 8, 0x03, 0xe8, b'n', b'o', b'r', b'm', b'a', b'l']
    );
}

#[test]
fn websocket_peer_close_preserves_empty_reason_on_receive_and_prepare() {
    let payload = [0x03, 0xe9];

    let (mut transport, mut socket) = attached_transport_for_close();
    let receive = close_output(&mut transport, &mut socket, &masked(true, 8, &payload));
    assert_close_output(&receive, Some(1001), b"");
    assert_eq!(encode_close(&receive), [0x88, 2, 0x03, 0xe9]);

    let (mut transport, mut socket) = attached_transport_for_close();
    let prepared =
        prepared_close_output(&mut transport, &mut socket, &masked(true, 8, &payload));
    assert_close_output(&prepared, Some(1001), b"");
    assert_eq!(encode_close(&prepared), [0x88, 2, 0x03, 0xe9]);
}

#[test]
fn websocket_peer_close_rejects_malformed_and_invalid_payloads_on_both_paths() {
    for payload in [
        &[0x03][..],
        &[0x00, 0x01][..],
        &[0x03, 0xe8, 0xff][..],
    ] {
        let (mut transport, mut socket) = attached_transport_for_close();
        let receive = close_output(&mut transport, &mut socket, &masked(true, 8, payload));
        assert_close_output(&receive, Some(1002), b"");
        assert_eq!(encode_close(&receive), [0x88, 2, 0x03, 0xea]);

        let (mut transport, mut socket) = attached_transport_for_close();
        let prepared =
            prepared_close_output(&mut transport, &mut socket, &masked(true, 8, payload));
        assert_close_output(&prepared, Some(1002), b"");
        assert_eq!(encode_close(&prepared), [0x88, 2, 0x03, 0xea]);
    }
}

#[test]
fn websocket_close_payloads_require_valid_codes_and_utf8_reasons() {
    for payload in [
        vec![0x03],
        vec![0x03, 0xed],
        vec![0x03, 0xec],
        vec![0x03, 0xe8, 0xff],
    ] {
        let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
        let mut socket = WebSocketState::new([5; 16]);
        assert_eq!(
            block_on(transport.receive(&mut socket, 2, &masked(true, 8, &payload))),
            Ok(vec![WebSocketOutput::Close {
                code: Some(1002),
                reason: Vec::new(),
            }])
        );
    }
}

#[test]
fn websocket_input_rejects_noncanonical_extended_lengths() {
    let cases = [
        (126_u8, 125_u64, vec![7; 125]),
        (127_u8, 126_u64, vec![7; 126]),
        (127_u8, u64::from(u16::MAX), vec![7; u16::MAX as usize]),
    ];
    for (length_code, encoded_length, body) in cases {
        let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
        let mut socket = WebSocketState::new([5; 16]);
        assert_eq!(
            block_on(transport.receive(
                &mut socket,
                2,
                &masked_with_length_code(true, 2, &body, length_code, encoded_length),
            )),
            Ok(vec![WebSocketOutput::Close {
                code: Some(1002),
                reason: Vec::new(),
            }])
        );
    }

    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    assert_eq!(
        block_on(transport.receive(
            &mut socket,
            2,
            &masked_with_length_code(true, 2, &[], 127, 1_u64 << 63),
        )),
        Ok(vec![WebSocketOutput::Close {
            code: Some(1002),
            reason: Vec::new(),
        }])
    );
}

#[test]
fn websocket_input_processes_coalesced_frames_with_per_frame_limits() {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    let mut host_limits = Limits::default();
    host_limits.protocol.max_message_bytes = 16 * 1024 * 1024;
    let mut transport = LiveTransport::new(
        LiveHost::new(
            host_limits,
            boundary,
            Serving::new(ServingLimits::default()).unwrap(),
        )
        .unwrap(),
        TransportLimits {
            max_frame_bytes: 16 * 1024 * 1024,
            ..TransportLimits::default()
        },
    )
    .unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    let mut bytes = masked(true, 9, &[1]);
    bytes.extend(masked(true, 9, &[2]));
    bytes.extend(masked(true, 9, &[3]));

    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &bytes)),
        Ok(vec![
            WebSocketOutput::Pong(vec![1]),
            WebSocketOutput::Pong(vec![2]),
            WebSocketOutput::Pong(vec![3]),
        ])
    );
}

#[test]
fn websocket_input_rejects_oversized_control_frames_as_invalid() {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    let mut host_limits = Limits::default();
    host_limits.protocol.max_message_bytes = 16 * 1024 * 1024;
    let mut transport = LiveTransport::new(
        LiveHost::new(
            host_limits,
            boundary,
            Serving::new(ServingLimits::default()).unwrap(),
        )
        .unwrap(),
        TransportLimits {
            max_frame_bytes: 16 * 1024 * 1024,
            ..TransportLimits::default()
        },
    )
    .unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    let frame = masked_with_length_code(true, 9, &[7; 126], 126, 126);

    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &frame)),
        Ok(vec![WebSocketOutput::Close {
            code: Some(1002),
            reason: Vec::new(),
        }])
    );
}

#[test]
fn websocket_input_malformed_frame_emits_protocol_close_once() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    let malformed = masked(true, 0, b"");

    let output = block_on(transport.receive(&mut socket, 2, &malformed)).unwrap();
    assert_eq!(
        output,
        vec![WebSocketOutput::Close {
            code: Some(1002),
            reason: Vec::new(),
        }]
    );
    assert_eq!(
        encode_websocket_output(&output[0], TransportLimits::default()).unwrap(),
        Some(vec![0x88, 2, 0x03, 0xea])
    );
    assert_eq!(transport.take_retired_attachments(), Vec::<[u8; 16]>::new());
    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &masked(true, 9, b"p"))),
        Err(Error::Closed)
    );
}

#[test]
fn websocket_input_malformed_application_message_closes_and_retires_attachment() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"{}"}}"#,
                uuid(2),
                SUBPROTOCOL
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let credential = token(&created);
    assert_eq!(
        block_on(transport.upgrade(websocket_upgrade(1, &credential), [5; 16], 1)).status,
        101
    );

    let mut socket = WebSocketState::new([5; 16]);
    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &masked(true, 2, &[0xff]))),
        Ok(vec![WebSocketOutput::Close {
            code: Some(1002),
            reason: Vec::new(),
        }])
    );
    assert_eq!(
        block_on(transport.close_attachment([5; 16], 2)),
        Err(Error::Closed)
    );
}

#[test]
fn websocket_input_coalesced_malformed_frame_closes_after_prior_output() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    let mut bytes = masked(true, 9, b"p");
    bytes.extend(masked(true, 0, b""));

    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &bytes)),
        Ok(vec![
            WebSocketOutput::Pong(vec![b'p']),
            WebSocketOutput::Close {
                code: Some(1002),
                reason: Vec::new(),
            },
        ])
    );
}

#[test]
fn websocket_input_limit_emits_message_too_big_close() {
    let boundary = SessionBoundary::new(OriginPolicy::new([origin()], []), 10);
    let mut host_limits = Limits::default();
    host_limits.protocol.max_message_bytes = 16 * 1024 * 1024;
    let host = LiveHost::new(
        host_limits,
        boundary,
        Serving::new(ServingLimits::default()).unwrap(),
    )
    .unwrap();
    let limits = TransportLimits {
        max_frame_bytes: 16 * 1024 * 1024,
        max_outgoing_bytes: 256,
        ..TransportLimits::default()
    };
    let mut transport = LiveTransport::new(host, limits).unwrap();
    let mut socket = WebSocketState::new([5; 16]);
    let frame = {
        let body = vec![7; 16 * 1024 * 1024 + 1];
        masked_with_length_code(true, 2, &body, 127, body.len() as u64)
    };
    let output = block_on(transport.receive(&mut socket, 2, &frame)).unwrap();
    assert_eq!(
        output,
        vec![WebSocketOutput::Close {
            code: Some(1009),
            reason: Vec::new(),
        }]
    );
    assert_eq!(
        encode_websocket_output(&output[0], limits).unwrap(),
        Some(vec![0x88, 2, 0x03, 0xf1])
    );
    assert_eq!(
        block_on(transport.receive(&mut socket, 2, &masked(true, 9, b"p"))),
        Err(Error::Closed)
    );
}

#[test]
fn application_responses_reach_the_websocket_as_canonical_binary() {
    let mut transport = LiveTransport::new(host(), TransportLimits::default()).unwrap();
    let mut issuer = Issuer(1, None);
    let mut authority = Authority;
    let mut deletion = Delete(true);
    let created = block_on(transport.handle(
        wire(
            "POST",
            "/orna/session",
            &format!(
                r#"{{"database":"{}","protocol":"orna.present.v1"}}"#,
                uuid(2)
            ),
        ),
        0,
        &mut authority,
        &mut issuer,
        &mut deletion,
    ));
    let cookie = token(&created);
    let mut upgrade = wire("GET", "/orna/live/01010101-0101-0101-0101-010101010101", "");
    upgrade.headers.extend([
        ("connection".into(), "Upgrade".into()),
        ("upgrade".into(), "websocket".into()),
        ("sec-websocket-version".into(), "13".into()),
        (
            "sec-websocket-key".into(),
            "dGhlIHNhbXBsZSBub25jZQ==".into(),
        ),
        ("sec-websocket-protocol".into(), SUBPROTOCOL.into()),
        ("cookie".into(), format!("orna_session={cookie}")),
    ]);
    assert_eq!(block_on(transport.upgrade(upgrade, [5; 16], 1)).status, 101);

    let mut socket = WebSocketState::new([5; 16]);
    let mut application = UnitApplication::default();
    let output = block_on(transport.receive_with_application(
        &mut socket,
        2,
        &masked(true, 2, &unsubscribe()),
        &mut application,
    ))
    .unwrap();
    assert_eq!(output.len(), 1);
    let WebSocketOutput::Binary { outcome, payload } = &output[0] else {
        panic!("application response must be a binary WebSocket output");
    };
    assert_eq!(*outcome, FrameOutcome::Accepted);
    let response = Envelope::decode(payload, Limits::default().protocol).unwrap();
    assert!(matches!(response.message, Message::Result { .. }));
}
