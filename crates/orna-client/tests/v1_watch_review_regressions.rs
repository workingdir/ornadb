//! Executable review evidence for ornadb-gov5.17.2 / GitHub #790.
//!
//! Normative authority: reference/Orna-1.0.0/Orna-1.0.0.md, sections 14.4
//! (ORNA-WIRE-001/002/003/006), 30.3, 30.4, 30.5 (ORNA-PROTO-003), and 30.7.
//! Revisions belong to a watch. A fresh watch starts at zero, accepts its own
//! complete tree, and then applies matching-base deltas atomically. Subscribe
//! responses echo their originating request; separate subscriptions use distinct
//! request IDs here, avoiding request replay as a confounding explanation.
//!
//! Ignored regressions assert required behavior, not the known implementation
//! gap. Execute them explicitly for review; an ignored result is not a pass.
//! These bounded in-memory tests exercise public bootstrap/driver/renderer APIs.
//! They do not exercise HTTP, authentication, server request retention, or token
//! rotation/cancellation. No filesystem or temporary directories are used.

use orna_client::{
    AuthenticatedLiveTransport, BootstrappedLiveAttachment, LiveByteDriver, LiveSessionDriver,
    LiveSessionEvent, PrefetchedBinaryTransport, PresentRenderer, RequestIdAllocator,
};
use orna_protocol_v1::{
    CanonicalSnapshot, Envelope, Limits, Message, PresentNode, PresentationContext,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

const WATCH_A: [u8; 16] = [7; 16];
const WATCH_B: [u8; 16] = [8; 16];
const REQUEST_A: [u8; 16] = [1; 16];
const REQUEST_B: [u8; 16] = [2; 16];

#[derive(Default)]
struct Traffic {
    sent: Vec<Vec<u8>>,
    reads: usize,
}

struct MockTransport {
    incoming: VecDeque<Vec<u8>>,
    traffic: Rc<RefCell<Traffic>>,
}

impl LiveByteDriver for MockTransport {
    type Error = &'static str;

    fn receive_binary<'a>(
        &'a mut self,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Self::Error>> + 'a>> {
        Box::pin(async move {
            self.traffic.borrow_mut().reads += 1;
            let bytes = self.incoming.pop_front().ok_or("no queued frame")?;
            if bytes.len() > max_bytes {
                return Err("frame exceeds negotiated limit");
            }
            Ok(bytes)
        })
    }

    fn send_binary<'a>(
        &'a mut self,
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + 'a>> {
        Box::pin(async move {
            self.traffic.borrow_mut().sent.push(bytes);
            Ok(())
        })
    }
}

impl AuthenticatedLiveTransport for MockTransport {}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Publication {
    watch: [u8; 16],
    revision: u64,
    snapshot: CanonicalSnapshot,
    present: PresentNode,
}

#[derive(Clone, Default)]
struct Renderer(Rc<RefCell<Vec<Publication>>>);

impl PresentRenderer for Renderer {
    type Error = &'static str;

    fn publish(
        &mut self,
        watch: [u8; 16],
        revision: u64,
        snapshot: &CanonicalSnapshot,
        present: &PresentNode,
    ) -> Result<(), Self::Error> {
        self.0.borrow_mut().push(Publication {
            watch,
            revision,
            snapshot: snapshot.clone(),
            present: present.clone(),
        });
        Ok(())
    }
}

struct Requests(u128);

impl RequestIdAllocator for Requests {
    fn next_request_id(&mut self) -> [u8; 16] {
        self.0 = self.0.checked_add(1).expect("request ID space");
        self.0.to_be_bytes()
    }
}

type Driver = LiveSessionDriver<PrefetchedBinaryTransport<MockTransport>, Renderer, Requests>;

// Every mock operation is immediately ready. Fail promptly if that changes;
// never busy-spin or depend on a timer/runtime to bound these regressions.
fn ready<F: Future>(future: F) -> F::Output {
    let mut context = Context::from_waker(Waker::noop());
    match Box::pin(future).as_mut().poll(&mut context) {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("in-memory operation unexpectedly suspended"),
    }
}

fn subscribe(request: [u8; 16]) -> Envelope {
    Envelope {
        request: Some(request),
        watch: None,
        message: Message::Subscribe {
            resource: [3; 16],
            presentation: PresentationContext {
                locale: "en-GB".into(),
                timezone: None,
                width: Some(80),
                theme: "terminal/default".into(),
                supported_kinds: vec!["text".into()],
            },
        },
        extensions: BTreeMap::new(),
    }
}

// Small canonical CBOR builders follow live_bootstrap.rs and
// live_presentation_reconnect.rs. Opaque Present/patch values enter through the
// public decoder; expected trees are independently supplied complete snapshots.
fn cbor_text(text: &str) -> Vec<u8> {
    assert!(text.len() < 24);
    let mut bytes = vec![0x60 + text.len() as u8];
    bytes.extend(text.as_bytes());
    bytes
}

fn pinned_snapshot() -> Vec<u8> {
    let mut bytes = vec![0x84, 0x01, 0xd8, 0x25, 0x50];
    bytes.extend([1; 16]);
    bytes.extend(cbor_text("sha256"));
    bytes.extend([0x58, 32]);
    bytes.extend([2; 32]);
    bytes
}

fn frame(code: u8, watch: [u8; 16], request: Option<[u8; 16]>, body: Vec<u8>) -> Vec<u8> {
    let mut bytes = vec![0xa5, 0x00, 0x01, 0x01, code, 0x02];
    if let Some(request) = request {
        bytes.push(0x50);
        bytes.extend(request);
    } else {
        bytes.push(0xf6);
    }
    bytes.extend([0x03, 0x50]);
    bytes.extend(watch);
    bytes.push(0x04);
    bytes.extend(body);
    Envelope::decode(&bytes, Limits::default())
        .expect("valid canonical review frame")
        .encode(Limits::default())
        .expect("review frame re-encodes")
}

fn snapshot(watch: [u8; 16], request: Option<[u8; 16]>, revision: u8, text: &str) -> Vec<u8> {
    assert!(revision < 24);
    let mut body = vec![0xa3, 0x00, revision, 0x01, 0xd9, 0xea, 0x6c, 0x84];
    body.extend(cbor_text("text"));
    body.extend([0xf6, 0xa1]);
    body.extend(cbor_text("text"));
    body.extend(cbor_text(text));
    body.extend([0x80, 0x02]);
    body.extend(pinned_snapshot());
    frame(16, watch, request, body)
}

fn delta(watch: [u8; 16], base: u8, next: u8, text: &str) -> Vec<u8> {
    assert!(base < next && next < 24);
    // One replace operation at the existing "text" property.
    let mut body = vec![
        0xa4, 0x00, base, 0x01, next, 0x02, 0x81, 0x83, 0x02, 0x81, 0x82, 0x00,
    ];
    body.extend(cbor_text("text"));
    body.extend(cbor_text(text));
    body.push(0x03);
    body.extend(pinned_snapshot());
    frame(17, watch, None, body)
}

fn bootstrap(
    request: [u8; 16],
    frames: Vec<Vec<u8>>,
    traffic: &Rc<RefCell<Traffic>>,
) -> BootstrappedLiveAttachment<MockTransport> {
    ready(BootstrappedLiveAttachment::start(
        MockTransport {
            incoming: frames.into(),
            traffic: Rc::clone(traffic),
        },
        subscribe(request),
        Limits::default(),
    ))
    .expect("correlated subscription snapshot admitted")
}

fn assert_publication(driver: &Driver, renderer: &Renderer, expected: &[u8]) {
    let envelope = Envelope::decode(expected, Limits::default()).unwrap();
    let Message::Snapshot {
        revision,
        present,
        snapshot,
    } = envelope.message
    else {
        panic!("expected a complete snapshot oracle");
    };
    let watch = envelope.watch.unwrap();
    assert_eq!(driver.watch(), watch);
    assert_eq!(driver.presentation().watch(), watch);
    let published = driver.presentation().published().expect("published tree");
    assert_eq!(published.revision(), revision);
    assert_eq!(published.present(), &present);
    assert_eq!(published.snapshot(), &snapshot);
    assert!(!driver.presentation().awaiting_snapshot());
    assert!(driver.presentation().take_resync_request().is_none());
    assert_eq!(
        renderer.0.borrow().last(),
        Some(&Publication {
            watch,
            revision,
            snapshot,
            present,
        }),
        "renderer must receive the complete accepted tree under its own watch"
    );
}

fn fresh_watch_transition(old_revision: u8, replacement_text: &str) {
    assert_ne!(WATCH_A, WATCH_B);
    assert_ne!(REQUEST_A, REQUEST_B);
    let traffic_a = Rc::new(RefCell::new(Traffic::default()));
    let traffic_b = Rc::new(RefCell::new(Traffic::default()));
    let renderer = Renderer::default();
    let mut frames = vec![snapshot(WATCH_A, Some(REQUEST_A), 0, "before")];
    if old_revision != 0 {
        // A begins at zero, then receives a coalesced automatic snapshot at 5.
        frames.push(snapshot(WATCH_A, None, old_revision, "before"));
    }
    let mut driver = bootstrap(REQUEST_A, frames, &traffic_a)
        .into_driver(renderer.clone(), Requests(100))
        .unwrap();
    assert_eq!(
        ready(driver.receive_once()).unwrap(),
        LiveSessionEvent::SnapshotPublished { revision: 0 }
    );
    if old_revision != 0 {
        assert_eq!(
            ready(driver.receive_once()).unwrap(),
            LiveSessionEvent::SnapshotPublished {
                revision: u64::from(old_revision)
            }
        );
    }
    assert_publication(
        &driver,
        &renderer,
        &snapshot(WATCH_A, None, old_revision, "before"),
    );
    let previous_publications = renderer.0.borrow().clone();
    let fresh = snapshot(WATCH_B, Some(REQUEST_B), 0, replacement_text);
    let replacement = bootstrap(
        REQUEST_B,
        vec![fresh.clone(), delta(WATCH_B, 0, 1, "after delta")],
        &traffic_b,
    );
    replacement.replace_driver(&mut driver).unwrap();
    assert_eq!(driver.watch(), WATCH_B);
    assert_eq!(*renderer.0.borrow(), previous_publications);
    assert!(driver.presentation().awaiting_snapshot());

    assert_eq!(
        ready(driver.receive_once()).unwrap(),
        LiveSessionEvent::SnapshotPublished { revision: 0 },
        "a fresh watch's complete revision-zero snapshot must be accepted independently of A"
    );
    assert_publication(&driver, &renderer, &fresh);
    assert_eq!(renderer.0.borrow().len(), previous_publications.len() + 1);
    assert_eq!(traffic_b.borrow().reads, 1, "consume prefetched B once");

    assert_eq!(
        ready(driver.receive_once()).unwrap(),
        LiveSessionEvent::DeltaPublished { revision: 1 },
        "B's first delta must use B's accepted revision-zero base"
    );
    assert_publication(
        &driver,
        &renderer,
        &snapshot(WATCH_B, None, 1, "after delta"),
    );
    assert_eq!(renderer.0.borrow().len(), previous_publications.len() + 2);
    assert_eq!(traffic_b.borrow().reads, 2);
    for (traffic, request) in [(&traffic_a, REQUEST_A), (&traffic_b, REQUEST_B)] {
        assert_eq!(
            traffic.borrow().sent,
            vec![subscribe(request).encode(Limits::default()).unwrap()],
            "valid snapshots and deltas must not cause a resync request"
        );
    }
}

#[test]
#[ignore = "Known Orna 1.0.0 gap: #790; run explicitly for review"]
fn fresh_watch_a_rev5_to_b_rev0_accepts_snapshot_and_delta() {
    // Keep the tree and pin equal so only the watch-local revision reset varies.
    fresh_watch_transition(5, "before");
}

#[test]
#[ignore = "Known Orna 1.0.0 gap: #790; run explicitly for review"]
fn fresh_watch_a0_to_changed_b0_accepts_snapshot_and_delta() {
    // Keep the revisions and pin equal so only the fresh watch's tree varies.
    fresh_watch_transition(0, "replacement");
}

#[test]
fn fresh_watch_a0_to_unchanged_b0_accepts_snapshot_and_delta_control() {
    fresh_watch_transition(0, "before");
}

#[test]
fn initial_watch_b0_accepts_snapshot_and_delta_control() {
    let traffic = Rc::new(RefCell::new(Traffic::default()));
    let renderer = Renderer::default();
    let first = snapshot(WATCH_B, Some(REQUEST_B), 0, "replacement");
    let mut driver = bootstrap(
        REQUEST_B,
        vec![first.clone(), delta(WATCH_B, 0, 1, "after delta")],
        &traffic,
    )
    .into_driver(renderer.clone(), Requests(100))
    .unwrap();
    assert_eq!(
        ready(driver.receive_once()).unwrap(),
        LiveSessionEvent::SnapshotPublished { revision: 0 }
    );
    assert_publication(&driver, &renderer, &first);
    assert_eq!(
        ready(driver.receive_once()).unwrap(),
        LiveSessionEvent::DeltaPublished { revision: 1 }
    );
    assert_publication(
        &driver,
        &renderer,
        &snapshot(WATCH_B, None, 1, "after delta"),
    );
    assert_eq!(renderer.0.borrow().len(), 2);
    assert_eq!(traffic.borrow().reads, 2);
    assert_eq!(
        traffic.borrow().sent,
        vec![subscribe(REQUEST_B).encode(Limits::default()).unwrap()]
    );
}

#[test]
fn existing_watch_rev5_to_rev0_requires_resync_control() {
    let traffic = Rc::new(RefCell::new(Traffic::default()));
    let renderer = Renderer::default();
    let mut driver = bootstrap(
        REQUEST_A,
        vec![
            snapshot(WATCH_A, Some(REQUEST_A), 0, "before"),
            snapshot(WATCH_A, None, 5, "current"),
            snapshot(WATCH_A, None, 0, "stale"),
        ],
        &traffic,
    )
    .into_driver(renderer.clone(), Requests(100))
    .unwrap();
    assert_eq!(
        ready(driver.receive_once()).unwrap(),
        LiveSessionEvent::SnapshotPublished { revision: 0 }
    );
    assert_eq!(
        ready(driver.receive_once()).unwrap(),
        LiveSessionEvent::SnapshotPublished { revision: 5 }
    );
    assert_publication(&driver, &renderer, &snapshot(WATCH_A, None, 5, "current"));
    let previous = driver.presentation().published().cloned();
    let previous_publications = renderer.0.borrow().clone();
    let LiveSessionEvent::ResyncSent { request } = ready(driver.receive_once()).unwrap() else {
        panic!("resetting an existing watch must request resynchronisation");
    };
    assert_eq!(request.watch(), WATCH_A);
    assert!(driver.presentation().awaiting_snapshot());
    assert_eq!(driver.presentation().published().cloned(), previous);
    assert_eq!(*renderer.0.borrow(), previous_publications);
    let sent = &traffic.borrow().sent;
    assert_eq!(sent.len(), 2);
    let resync = Envelope::decode(&sent[1], Limits::default()).unwrap();
    assert_eq!(resync.watch, Some(WATCH_A));
    assert_eq!(resync.message, Message::Resync);
    assert!(resync.request.is_some());
    assert_ne!(resync.request, Some(REQUEST_A));
    assert_ne!(resync.request, Some(REQUEST_B));
}
