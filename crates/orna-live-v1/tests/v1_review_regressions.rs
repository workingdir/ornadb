//! Executable review evidence for ornadb-gov5.17.3 / GitHub issue #789.
//!
//! Normative authority: immutable Orna 1.0.0, source/10-execution.md,
//! ORNA-CONCUR-001 and TASK-END-1 steps 1, 3, 4 and 7; and
//! source/30-protocol.md, "Transport and value profile", incorporating
//! RFC 6455 sections 5.5.1-5.5.3 for Close and Ping/Pong.
//!
//! Ignored regressions assert required behavior and must be run explicitly
//! for review. Skipping them is not evidence of conformance. These public API
//! tests cover the supervisor and the concurrent transport preparation seam,
//! not the executable server's socket-reading loop or durable request Cancel.
//! All fixtures are in memory. Explicit polling and notification barriers
//! avoid clocks, sleeps, background workers, and filesystem dependencies.

use futures::executor::block_on;
use orna_live_v1::{
    CreateRequest, Error, Limits, LiveApplicationWorkLease, LiveApplicationWorkSupervisor,
    LiveCredentialIssuer, LiveHost, LiveTransport, ResumeRequest, TransportLimits,
    WebSocketApplicationPreparation, WebSocketOutput, WebSocketState, encode_websocket_output,
};
use orna_protocol_v1::{Envelope, Message, PresentationContext};
use orna_security_v1::{BoundaryError, CredentialIssuer, Origin, OriginPolicy, SessionBoundary};
use orna_serving_v1::{Limits as ServingLimits, Serving};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, mpsc},
    task::{Context, Poll, Wake, Waker},
};

struct WakeNotification(mpsc::Sender<()>);

impl Wake for WakeNotification {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        // A failed assertion may already have dropped the receiving probe.
        let _ = self.0.send(());
    }
}

struct JoinProbe {
    waker: Waker,
    notifications: mpsc::Receiver<()>,
}

impl JoinProbe {
    fn new() -> Self {
        let (sender, notifications) = mpsc::channel();
        Self {
            waker: Waker::from(Arc::new(WakeNotification(sender))),
            notifications,
        }
    }

    fn poll<F: Future + ?Sized>(&self, future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(&self.waker))
    }

    fn assert_notified(&self) {
        // complete() has already returned on this thread. No elapsed-time
        // assumption or blocking wait is needed to observe its notification.
        self.notifications
            .try_recv()
            .expect("lease acknowledgement must wake the pending join");
    }
}

fn assert_cancellation_delivered(lease: &mut LiveApplicationWorkLease) {
    assert!(
        lease.is_cancelled(),
        "every unfinished lease must be signalled"
    );
    assert_eq!(
        lease.cancellation().try_recv(),
        Ok(Some(())),
        "cancellation must reach the worker notification receiver"
    );
    assert_eq!(lease.check_active(), Err(Error::Closed));
    assert!(matches!(lease.spawn_child(), Err(Error::Closed)));
}

/// ORNA-CONCUR-001; TASK-END-1 steps 1, 3, 4 and 7. A failed first session
/// cannot exempt later sessions or descendants from cancellation and joining.
#[test]
#[ignore = "Known Orna 1.0.0 gap: #789; run explicitly for review"]
fn cancel_and_join_all_signals_and_joins_later_sessions_after_failed_lease() {
    let supervisor = LiveApplicationWorkSupervisor::new();
    // Deliberately ordered identities: the failed session is visited first.
    drop(supervisor.admit([1; 16], [1; 16]).unwrap());
    let mut later = supervisor.admit([2; 16], [2; 16]).unwrap();
    let mut descendant = later.spawn_child().unwrap();
    let mut last = supervisor.admit([3; 16], [3; 16]).unwrap();
    let mut drain = supervisor.cancel_and_join_all();
    let probe = JoinProbe::new();
    let initial = probe.poll(drain.as_mut());

    // Baseline #789 returns DeletionFailed here without signalling later.
    // These are normative assertions, not assertions accepting that defect.
    assert_cancellation_delivered(&mut later);
    assert_cancellation_delivered(&mut descendant);
    assert_cancellation_delivered(&mut last);
    assert_eq!(initial, Poll::Pending, "drain must await acknowledgement");
    for session in [[1; 16], [2; 16], [3; 16]] {
        assert!(matches!(
            supervisor.admit(session, [4; 16]),
            Err(Error::Closed)
        ));
    }

    later.complete();
    assert_eq!(
        probe.poll(drain.as_mut()),
        Poll::Pending,
        "acknowledging a root must not acknowledge its unfinished descendant"
    );
    descendant.complete();
    let last_probe = JoinProbe::new();
    assert_eq!(
        last_probe.poll(drain.as_mut()),
        Poll::Pending,
        "the retained earlier error must wait for the final session too"
    );
    last.complete();
    last_probe.assert_notified();
    assert_eq!(
        last_probe.poll(drain.as_mut()),
        Poll::Ready(Err(Error::DeletionFailed)),
        "report the failed lease only after all other work has acknowledged"
    );
}

/// Passing control for the same cancellation, fencing, and join barriers.
#[test]
fn cancel_and_join_all_waits_for_recursive_acknowledgements() {
    let supervisor = LiveApplicationWorkSupervisor::new();
    let mut root = supervisor.admit([1; 16], [1; 16]).unwrap();
    let mut descendant = root.spawn_child().unwrap();
    assert!(!root.is_cancelled());
    assert!(!descendant.is_cancelled());
    let mut drain = supervisor.cancel_and_join_all();
    let probe = JoinProbe::new();
    assert_eq!(probe.poll(drain.as_mut()), Poll::Pending);
    assert_cancellation_delivered(&mut root);
    assert_cancellation_delivered(&mut descendant);
    assert!(matches!(
        supervisor.admit([1; 16], [2; 16]),
        Err(Error::Closed)
    ));

    root.complete();
    let descendant_probe = JoinProbe::new();
    assert_eq!(descendant_probe.poll(drain.as_mut()), Poll::Pending);
    descendant.complete();
    descendant_probe.assert_notified();
    assert_eq!(descendant_probe.poll(drain.as_mut()), Poll::Ready(Ok(())));
}

/// An unacknowledged drop is a failure, not successful child termination.
#[test]
fn cancel_and_join_all_reports_an_unacknowledged_lease() {
    let supervisor = LiveApplicationWorkSupervisor::new();
    drop(supervisor.admit([1; 16], [1; 16]).unwrap());
    let mut drain = supervisor.cancel_and_join_all();
    assert_eq!(
        JoinProbe::new().poll(drain.as_mut()),
        Poll::Ready(Err(Error::DeletionFailed))
    );
    assert!(matches!(
        supervisor.admit([1; 16], [2; 16]),
        Err(Error::Closed)
    ));
}

// Synthetic in-memory credential source; no real credential is read or logged.
struct FixtureIssuer;

impl CredentialIssuer for FixtureIssuer {
    fn issue_credential(&mut self) -> Result<[u8; 32], BoundaryError> {
        Ok([1; 32])
    }
}

impl LiveCredentialIssuer for FixtureIssuer {
    fn last_issued(&self) -> Option<[u8; 32]> {
        Some([1; 32])
    }
}

fn attached_transport() -> (LiveTransport, WebSocketState) {
    let origin = Origin::parse("https://app.example").unwrap();
    let limits = Limits::default();
    let mut host = LiveHost::new(
        limits,
        SessionBoundary::new(OriginPolicy::new([origin.clone()], []), 10),
        Serving::new(ServingLimits::default()).unwrap(),
    )
    .unwrap();
    let subscribe = Envelope {
        request: Some([2; 16]),
        watch: None,
        message: Message::Subscribe {
            resource: [3; 16],
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
    .encode(limits.protocol)
    .unwrap();
    // Logical API timestamps only: no clock advances or lease-expiry races.
    let credential = block_on(host.create(
        CreateRequest {
            id: [1; 16],
            origin: origin.clone(),
            expires_at: 100,
            now: 0,
            subscribe: &subscribe,
        },
        &mut FixtureIssuer,
    ))
    .unwrap();
    block_on(host.resume(ResumeRequest {
        id: [1; 16],
        origin: &origin,
        credential: &credential,
        attachment: [4; 16],
        now: 1,
    }))
    .unwrap();
    (
        LiveTransport::new(host, TransportLimits::default()).unwrap(),
        WebSocketState::new([4; 16]),
    )
}

fn masked_control(opcode: u8, payload: &[u8]) -> Vec<u8> {
    assert!(matches!(opcode, 8..=10));
    assert!(payload.len() <= 125);
    // Fixed masking is solely a deterministic parser fixture, not a client.
    let mask = [0x12, 0x34, 0x56, 0x78];
    let mut frame = vec![0x80 | opcode, 0x80 | u8::try_from(payload.len()).unwrap()];
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(i, byte)| byte ^ mask[i % 4]),
    );
    frame
}

fn prepared_output(
    transport: &mut LiveTransport,
    socket: &mut WebSocketState,
    frame: &[u8],
) -> WebSocketOutput {
    match block_on(transport.prepare_websocket_application(socket, 2, frame)).unwrap() {
        WebSocketApplicationPreparation::Output(output) => output,
        WebSocketApplicationPreparation::Pending => panic!("complete control frame was deferred"),
        WebSocketApplicationPreparation::Work(_) => {
            panic!("control frame scheduled application work")
        }
    }
}

/// Chapter 30 transport profile; RFC 6455 section 5.5.1 requires a Close reply.
/// No application message or work is outstanding that could delay the reply.
#[test]
#[ignore = "Known Orna 1.0.0 gap: #789; run explicitly for review"]
fn prepare_websocket_peer_close_emits_closing_handshake() {
    let (mut transport, mut socket) = attached_transport();
    let output = prepared_output(&mut transport, &mut socket, &masked_control(8, &[]));
    let wire = encode_websocket_output(&output, TransportLimits::default())
        .unwrap()
        .expect("a received Close requires a wire Close reply, not a logical acknowledgement");
    assert!(matches!(output, WebSocketOutput::Close { .. }));
    // Require a final, unmasked Close frame without prescribing an optional
    // status code: RFC 6455 permits a code even when the peer supplied none.
    assert!(wire.len() >= 2);
    assert_eq!(wire[0], 0x88);
    assert_eq!(wire[1] & 0x80, 0);
    assert!(wire[1] <= 125);
    assert_eq!(wire.len(), 2 + usize::from(wire[1]));
}

/// Passing control: Chapter 30 and RFC 6455 sections 5.5.2-5.5.3 require
/// the Pong's application data to equal the received Ping's data.
#[test]
fn prepare_websocket_ping_emits_pong_with_identical_payload() {
    for payload in [&[][..], &[0, 0x7f, 0x80, 0xff][..], &[0x5a; 125][..]] {
        let (mut transport, mut socket) = attached_transport();
        let output = prepared_output(&mut transport, &mut socket, &masked_control(9, payload));
        assert_eq!(output, WebSocketOutput::Pong(payload.to_vec()));
        let wire = encode_websocket_output(&output, TransportLimits::default())
            .unwrap()
            .expect("Pong must produce wire bytes");
        assert_eq!(wire[0], 0x8a);
        assert_eq!(usize::from(wire[1]), payload.len());
        assert_eq!(&wire[2..], payload);
    }
}
