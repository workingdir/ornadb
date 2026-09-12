//! Bounded asynchronous ownership of one authenticated `orna.present.v1` watch.
//!
//! Authentication, TLS, WebSocket attachment, and protocol negotiation happen
//! before this module is constructed. `AuthenticatedLiveTransport` is an
//! explicit trust-boundary marker; it does not validate credentials itself.

use std::{future::Future, pin::Pin};

use orna_protocol_v1::{CanonicalSnapshot, Limits, PresentNode};

use crate::live_presentation::{
    LivePresentationError, LivePresentationUpdate, PublishedPresentation, ResyncRequest,
    WatchPresentation,
};

/// Async binary I/O for an already-authenticated and negotiated live
/// attachment. Implementations must bound their read before allocating; the
/// driver independently checks the returned payload length as defence in
/// depth when an adapter violates the requested limit.
pub trait LiveByteDriver {
    type Error;

    fn receive_binary<'a>(
        &'a mut self,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Self::Error>> + 'a>>;

    fn send_binary<'a>(
        &'a mut self,
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + 'a>>;
}

/// Trust-boundary marker for a transport whose caller has completed
/// authentication, live attachment, and `orna.present.v1` negotiation.
///
/// Implementing this trait does not authenticate the transport. It records the
/// required precondition in the client-library API and keeps credentials out
/// of this presentation lifecycle owner.
pub trait AuthenticatedLiveTransport: LiveByteDriver {}

/// The only renderer seam: the driver owns receive, state, resync, and
/// reconnect ordering; the consumer only receives accepted complete trees.
pub trait PresentRenderer {
    type Error;

    fn publish(
        &mut self,
        watch: [u8; 16],
        revision: u64,
        snapshot: &CanonicalSnapshot,
        present: &PresentNode,
    ) -> Result<(), Self::Error>;
}

/// Supplies session-scoped request IDs for outbound resync operations.
pub trait RequestIdAllocator {
    fn next_request_id(&mut self) -> [u8; 16];
}

/// Result of one driver receive/publish step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiveSessionEvent {
    SnapshotPublished { revision: u64 },
    DeltaPublished { revision: u64 },
    ResyncSent { request: ResyncRequest },
}

/// Errors from the authenticated live attachment, typed presentation owner,
/// or renderer publication boundary.
#[derive(Debug)]
pub enum LiveSessionError<I, R> {
    Io(I),
    Presentation(LivePresentationError),
    Protocol(orna_protocol_v1::Error),
    Renderer(R),
}

/// A concrete async live client driver for one authenticated watch.
///
/// It retains at most one received payload, one pending resync frame, and one
/// failed renderer publication. It never buffers a stream eagerly and never
/// replays a mutation or delta after attachment replacement.
pub struct LiveSessionDriver<I, R, A> {
    io: I,
    watch: [u8; 16],
    limits: Limits,
    presentation: WatchPresentation,
    renderer: R,
    request_ids: A,
    pending_resync: Option<(ResyncRequest, Vec<u8>)>,
    pending_publication: Option<(LivePresentationUpdate, PublishedPresentation)>,
}

impl<I, R, A> LiveSessionDriver<I, R, A>
where
    I: AuthenticatedLiveTransport,
    R: PresentRenderer,
    A: RequestIdAllocator,
{
    /// Constructs a driver only around a caller-authenticated, already
    /// negotiated `orna.present.v1` attachment. This function does not prove
    /// or perform authentication.
    pub fn new(
        io: I,
        watch: [u8; 16],
        limits: Limits,
        renderer: R,
        request_ids: A,
    ) -> Result<Self, LiveSessionError<I::Error, R::Error>> {
        let presentation =
            WatchPresentation::new(watch, limits).map_err(LiveSessionError::Presentation)?;
        Ok(Self {
            io,
            watch,
            limits,
            presentation,
            renderer,
            request_ids,
            pending_resync: None,
            pending_publication: None,
        })
    }

    pub fn presentation(&self) -> &WatchPresentation {
        &self.presentation
    }

    pub const fn watch(&self) -> [u8; 16] {
        self.watch
    }

    pub const fn limits(&self) -> Limits {
        self.limits
    }

    /// Receives at most one bounded binary envelope, publishes an accepted
    /// complete tree, or sends the current resync request. A failed send keeps
    /// the exact encoded bytes and request identity for the next call.
    pub async fn receive_once(
        &mut self,
    ) -> Result<LiveSessionEvent, LiveSessionError<I::Error, R::Error>> {
        if let Some(event) = self.flush_resync().await? {
            return Ok(event);
        }
        if let Some((update, published)) = self.pending_publication.clone() {
            self.publish(update, &published)?;
            return Ok(Self::publication_event(update, published.revision()));
        }

        let encoded = self
            .io
            .receive_binary(self.limits.max_message_bytes)
            .await
            .map_err(LiveSessionError::Io)?;
        let update = if encoded.len() > self.limits.max_message_bytes {
            // Do not pass an adapter-oversized payload to the decoder.
            self.presentation.receive_encoded(&[])
        } else {
            self.presentation.receive_encoded(&encoded)
        }
        .map_err(LiveSessionError::Presentation)?;
        match update {
            LivePresentationUpdate::SnapshotInstalled | LivePresentationUpdate::DeltaApplied => {
                let published = self
                    .presentation
                    .published()
                    .expect("accepted presentation update publishes state")
                    .clone();
                let event = Self::publication_event(update, published.revision());
                self.publish(update, &published)?;
                Ok(event)
            }
            LivePresentationUpdate::ResyncRequired => self
                .flush_resync()
                .await?
                .ok_or_else(|| unreachable!("resync intent must be created by a failed update")),
        }
    }

    /// Replaces the authenticated attachment itself. The old driver/stream is
    /// dropped, its queued frames cannot be consumed, and retained state is
    /// fenced until a complete snapshot arrives on the replacement.
    pub fn replace_authenticated_attachment(&mut self, io: I) {
        self.io = io;
        self.presentation.begin_resubscription();
        self.pending_resync = None;
        // A failed publication belongs to the old attachment. The retained
        // state remains available through `presentation()` but must not be
        // rendered ahead of the replacement attachment's complete snapshot.
        self.pending_publication = None;
    }

    pub(crate) fn replace_authenticated_attachment_with_watch(
        &mut self,
        io: I,
        watch: [u8; 16],
    ) -> Result<(), ()> {
        if watch == self.watch {
            return Err(());
        }
        self.io = io;
        self.watch = watch;
        let previous = self.presentation.published().cloned();
        let mut presentation = WatchPresentation::new(watch, self.limits).map_err(|_| ())?;
        if let Some(previous) = previous {
            let _ = presentation.install_snapshot(
                previous.revision(),
                previous.present().clone(),
                previous.snapshot().clone(),
            );
        }
        self.presentation = presentation;
        self.presentation.begin_resubscription();
        self.pending_resync = None;
        self.pending_publication = None;
        Ok(())
    }

    async fn flush_resync(
        &mut self,
    ) -> Result<Option<LiveSessionEvent>, LiveSessionError<I::Error, R::Error>> {
        if self.pending_resync.is_none() {
            if let Some(request) = self.presentation.take_resync_request() {
                let encoded = request
                    .encode(self.request_ids.next_request_id(), self.limits)
                    .map_err(LiveSessionError::Protocol)?;
                self.pending_resync = Some((request, encoded));
            }
        }
        let Some((request, encoded)) = self.pending_resync.clone() else {
            return Ok(None);
        };
        self.io
            .send_binary(encoded)
            .await
            .map_err(LiveSessionError::Io)?;
        self.presentation.acknowledge_resync_request(request);
        self.pending_resync = None;
        Ok(Some(LiveSessionEvent::ResyncSent { request }))
    }

    fn publish(
        &mut self,
        update: LivePresentationUpdate,
        published: &PublishedPresentation,
    ) -> Result<(), LiveSessionError<I::Error, R::Error>> {
        match self.renderer.publish(
            self.watch,
            published.revision(),
            published.snapshot(),
            published.present(),
        ) {
            Ok(()) => {
                self.pending_publication = None;
                Ok(())
            }
            Err(error) => {
                self.pending_publication = Some((update, published.clone()));
                Err(LiveSessionError::Renderer(error))
            }
        }
    }

    fn publication_event(update: LivePresentationUpdate, revision: u64) -> LiveSessionEvent {
        match update {
            LivePresentationUpdate::SnapshotInstalled => {
                LiveSessionEvent::SnapshotPublished { revision }
            }
            LivePresentationUpdate::DeltaApplied => LiveSessionEvent::DeltaPublished { revision },
            LivePresentationUpdate::ResyncRequired => unreachable!("resync is not publishable"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orna_protocol_v1::{Envelope, Message};
    use std::{
        collections::VecDeque,
        task::{Context, Poll, Waker},
    };

    #[derive(Default)]
    struct MemoryIo {
        incoming: VecDeque<Vec<u8>>,
        sent: Vec<Vec<u8>>,
        requested_limits: Vec<usize>,
        fail_sends: usize,
    }

    impl LiveByteDriver for MemoryIo {
        type Error = &'static str;

        fn receive_binary<'a>(
            &'a mut self,
            max_bytes: usize,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Self::Error>> + 'a>> {
            self.requested_limits.push(max_bytes);
            Box::pin(async move { self.incoming.pop_front().ok_or("no incoming frame") })
        }

        fn send_binary<'a>(
            &'a mut self,
            bytes: Vec<u8>,
        ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + 'a>> {
            Box::pin(async move {
                if self.fail_sends > 0 {
                    self.fail_sends -= 1;
                    return Err("send failed");
                }
                self.sent.push(bytes);
                Ok(())
            })
        }
    }

    impl AuthenticatedLiveTransport for MemoryIo {}

    #[derive(Default)]
    struct Renderer {
        revisions: Vec<u64>,
        trees: Vec<PresentNode>,
        fail: usize,
    }

    impl PresentRenderer for Renderer {
        type Error = &'static str;

        fn publish(
            &mut self,
            _watch: [u8; 16],
            revision: u64,
            _snapshot: &CanonicalSnapshot,
            present: &PresentNode,
        ) -> Result<(), Self::Error> {
            if self.fail > 0 {
                self.fail -= 1;
                return Err("renderer failed");
            }
            self.revisions.push(revision);
            self.trees.push(present.clone());
            Ok(())
        }
    }

    #[derive(Default)]
    struct Allocator {
        next: u8,
        allocations: usize,
    }

    impl RequestIdAllocator for Allocator {
        fn next_request_id(&mut self) -> [u8; 16] {
            self.allocations += 1;
            self.next += 1;
            [self.next; 16]
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let mut context = Context::from_waker(Waker::noop());
        let mut future = Box::pin(future);
        loop {
            if let Poll::Ready(value) = future.as_mut().poll(&mut context) {
                return value;
            }
        }
    }

    fn frame(code: u8, watch: [u8; 16], body: Vec<u8>) -> Vec<u8> {
        let mut bytes = vec![0xa5, 0x00, 0x01, 0x01, code, 0x02, 0xf6, 0x03, 0x50];
        bytes.extend(watch);
        bytes.extend([0x04]);
        bytes.extend(body);
        Envelope::decode(&bytes, Limits::default())
            .expect("test frame is canonical")
            .encode(Limits::default())
            .expect("test frame re-encodes canonically")
    }

    fn snapshot(revision: u8) -> Vec<u8> {
        frame(16, [7; 16], snapshot_body(revision, "text"))
    }

    fn snapshot_body(revision: u8, property: &str) -> Vec<u8> {
        let mut body = vec![0xa3, 0x00, revision, 0x01, 0xd9, 0xea, 0x6c, 0x84, 0x64];
        body.extend(b"text");
        body.extend([0xf6, 0xa1]);
        body.extend(text("text"));
        body.extend(text(property));
        body.extend([0x80, 0x02, 0x84, 0x01, 0xd8, 0x25, 0x50]);
        body.extend([1; 16]);
        body.extend([0x66]);
        body.extend(b"sha256");
        body.extend([0x58, 32]);
        body.extend([2; 32]);
        body
    }

    fn text(value: &str) -> Vec<u8> {
        assert!(value.len() < 24);
        let mut encoded = vec![0x60 + value.len() as u8];
        encoded.extend(value.as_bytes());
        encoded
    }

    fn array(values: Vec<Vec<u8>>) -> Vec<u8> {
        assert!(values.len() < 24);
        let mut encoded = vec![0x80 + values.len() as u8];
        for value in values {
            encoded.extend(value);
        }
        encoded
    }

    fn replace_text(property: &str) -> Vec<u8> {
        let path = array(vec![array(vec![vec![0x00], text("text")])]);
        array(vec![vec![0x02], path, text(property)])
    }

    fn remove_property(property: &str) -> Vec<u8> {
        let path = array(vec![array(vec![vec![0x00], text(property)])]);
        array(vec![vec![0x01], path])
    }

    fn delta_with_operations(base: u8, next: u8, operations: Vec<Vec<u8>>) -> Vec<u8> {
        let mut body = vec![0xa4, 0x00, base, 0x01, next, 0x02];
        body.extend(array(operations));
        body.extend([0x03]);
        body.extend([0x84, 0x01, 0xd8, 0x25, 0x50]);
        body.extend([1; 16]);
        body.extend([0x66]);
        body.extend(b"sha256");
        body.extend([0x58, 32]);
        body.extend([2; 32]);
        frame(17, [7; 16], body)
    }

    fn delta(base: u8, next: u8, property: &str) -> Vec<u8> {
        delta_with_operations(base, next, vec![replace_text(property)])
    }

    fn snapshot_present(encoded: &[u8]) -> PresentNode {
        let envelope = Envelope::decode(encoded, Limits::default()).unwrap();
        let Message::Snapshot { present, .. } = envelope.message else {
            panic!("expected snapshot frame");
        };
        present
    }

    fn assert_resync_frame(encoded: &[u8]) {
        let envelope = Envelope::decode(encoded, Limits::default()).unwrap();
        assert_eq!(envelope.watch, Some([7; 16]));
        assert!(envelope.request.is_some());
        assert!(matches!(envelope.message, Message::Resync));
    }

    #[test]
    fn receives_real_snapshot_bytes_and_publishes_renderer_side_effect() {
        let mut io = MemoryIo::default();
        io.incoming.push_back(snapshot(0));
        let mut driver = LiveSessionDriver::new(
            io,
            [7; 16],
            Limits::default(),
            Renderer::default(),
            Allocator::default(),
        )
        .unwrap();
        assert!(matches!(
            block_on(driver.receive_once()),
            Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
        ));
        assert_eq!(driver.presentation().published().unwrap().revision(), 0);
        assert_eq!(driver.renderer.trees, vec![snapshot_present(&snapshot(0))]);
    }

    #[test]
    fn receives_snapshot_then_valid_delta_and_publishes_changed_tree() {
        let mut io = MemoryIo::default();
        io.incoming.push_back(snapshot(0));
        io.incoming.push_back(delta(0, 1, "changed"));
        let mut driver = LiveSessionDriver::new(
            io,
            [7; 16],
            Limits::default(),
            Renderer::default(),
            Allocator::default(),
        )
        .unwrap();

        assert!(matches!(
            block_on(driver.receive_once()),
            Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
        ));
        assert!(matches!(
            block_on(driver.receive_once()),
            Ok(LiveSessionEvent::DeltaPublished { revision: 1 })
        ));
        assert_eq!(driver.renderer.revisions, vec![0, 1]);
        assert_eq!(driver.renderer.trees[0], snapshot_present(&snapshot(0)));
        assert_eq!(
            driver.renderer.trees[1],
            snapshot_present(&frame(16, [7; 16], snapshot_body(1, "changed")))
        );
        assert_eq!(driver.presentation().published().unwrap().revision(), 1);
    }

    #[test]
    fn bad_base_and_mid_patch_failure_send_resync_without_changing_tree() {
        let mut io = MemoryIo::default();
        io.incoming.push_back(snapshot(0));
        io.incoming.push_back(delta(9, 10, "bad-base"));
        let mut driver = LiveSessionDriver::new(
            io,
            [7; 16],
            Limits::default(),
            Renderer::default(),
            Allocator::default(),
        )
        .unwrap();
        assert!(matches!(
            block_on(driver.receive_once()),
            Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
        ));
        let original = driver.presentation().published().cloned();
        assert!(matches!(
            block_on(driver.receive_once()),
            Ok(LiveSessionEvent::ResyncSent { .. })
        ));
        assert_resync_frame(&driver.io.sent[0]);
        assert_eq!(driver.presentation().published().cloned(), original);
        let sent = driver.io.sent.len();

        let malformed_io = MemoryIo::default();
        let mut malformed = LiveSessionDriver::new(
            malformed_io,
            [7; 16],
            Limits::default(),
            Renderer::default(),
            Allocator::default(),
        )
        .unwrap();
        malformed.presentation.install_snapshot(
            0,
            snapshot_present(&snapshot(0)),
            driver
                .presentation()
                .published()
                .unwrap()
                .snapshot()
                .clone(),
        );
        let original = malformed.presentation().published().cloned();
        malformed.io.incoming.push_back(delta_with_operations(
            0,
            1,
            vec![replace_text("first"), remove_property("missing")],
        ));
        assert!(matches!(
            block_on(malformed.receive_once()),
            Ok(LiveSessionEvent::ResyncSent { .. })
        ));
        assert_resync_frame(&malformed.io.sent[0]);
        assert_eq!(malformed.presentation().published().cloned(), original);
        assert_eq!(driver.io.sent.len(), sent);
    }

    #[test]
    fn wrong_watch_is_rejected_without_touching_published_tree_or_sending() {
        let mut io = MemoryIo::default();
        io.incoming.push_back(snapshot(0));
        let mut driver = LiveSessionDriver::new(
            io,
            [7; 16],
            Limits::default(),
            Renderer::default(),
            Allocator::default(),
        )
        .unwrap();
        block_on(driver.receive_once()).unwrap();
        let original = driver.presentation().published().cloned();
        driver
            .io
            .incoming
            .push_back(frame(16, [8; 16], snapshot_body(1, "wrong-watch")));
        assert!(matches!(
            block_on(driver.receive_once()),
            Err(LiveSessionError::Presentation(
                LivePresentationError::WrongWatch
            ))
        ));
        assert_eq!(driver.presentation().published().cloned(), original);
        assert!(driver.io.sent.is_empty());
    }

    #[test]
    fn failed_resync_send_retries_identical_bytes_and_request_identity() {
        let mut io = MemoryIo::default();
        io.incoming.push_back(vec![0xff]);
        io.fail_sends = 1;
        let mut driver = LiveSessionDriver::new(
            io,
            [7; 16],
            Limits::default(),
            Renderer::default(),
            Allocator::default(),
        )
        .unwrap();
        assert!(matches!(
            block_on(driver.receive_once()),
            Err(LiveSessionError::Io("send failed"))
        ));
        let request = driver.presentation().take_resync_request().unwrap();
        assert!(matches!(
            block_on(driver.receive_once()),
            Ok(LiveSessionEvent::ResyncSent { request: sent }) if sent == request
        ));
        assert_eq!(driver.request_ids.allocations, 1);
        assert_eq!(driver.io.sent.len(), 1);
        let decoded = Envelope::decode(&driver.io.sent[0], Limits::default()).unwrap();
        assert_eq!(decoded.request, Some([1; 16]));
        assert_eq!(decoded.watch, Some([7; 16]));
    }

    #[test]
    fn oversized_adapter_payload_is_rejected_before_decode() {
        let mut io = MemoryIo::default();
        io.incoming.push_back(vec![0; 129]);
        let limits = Limits {
            max_message_bytes: 128,
            ..Limits::default()
        };
        let mut driver = LiveSessionDriver::new(
            io,
            [7; 16],
            limits,
            Renderer::default(),
            Allocator::default(),
        )
        .unwrap();
        assert!(matches!(
            block_on(driver.receive_once()),
            Ok(LiveSessionEvent::ResyncSent { .. })
        ));
        assert_eq!(driver.io.requested_limits, vec![128]);
    }

    #[test]
    fn replacing_attachment_drops_old_queue_and_requires_new_complete_snapshot() {
        let mut old = MemoryIo::default();
        old.incoming.push_back(snapshot(0));
        old.incoming.push_back(vec![0xff]);
        let mut driver = LiveSessionDriver::new(
            old,
            [7; 16],
            Limits::default(),
            Renderer::default(),
            Allocator::default(),
        )
        .unwrap();
        assert!(matches!(
            block_on(driver.receive_once()),
            Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
        ));
        let mut replacement = MemoryIo::default();
        replacement.incoming.push_back(snapshot(1));
        driver.replace_authenticated_attachment(replacement);
        assert!(matches!(
            block_on(driver.receive_once()),
            Ok(LiveSessionEvent::SnapshotPublished { revision: 1 })
        ));
        assert_eq!(driver.presentation().published().unwrap().revision(), 1);
    }

    #[test]
    fn renderer_failure_keeps_latest_tree_and_retries_without_false_success() {
        let mut io = MemoryIo::default();
        io.incoming.push_back(snapshot(0));
        let mut renderer = Renderer::default();
        renderer.fail = 1;
        let mut driver = LiveSessionDriver::new(
            io,
            [7; 16],
            Limits::default(),
            renderer,
            Allocator::default(),
        )
        .unwrap();
        assert!(matches!(
            block_on(driver.receive_once()),
            Err(LiveSessionError::Renderer("renderer failed"))
        ));
        assert_eq!(driver.presentation().published().unwrap().revision(), 0);
        assert!(matches!(
            block_on(driver.receive_once()),
            Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
        ));
        assert_eq!(driver.renderer.revisions, vec![0]);
    }

    #[test]
    fn reconnect_drops_old_failed_publication_before_new_snapshot() {
        let mut old = MemoryIo::default();
        old.incoming.push_back(snapshot(0));
        let mut renderer = Renderer::default();
        renderer.fail = 1;
        let mut driver = LiveSessionDriver::new(
            old,
            [7; 16],
            Limits::default(),
            renderer,
            Allocator::default(),
        )
        .unwrap();
        assert!(matches!(
            block_on(driver.receive_once()),
            Err(LiveSessionError::Renderer("renderer failed"))
        ));
        assert_eq!(driver.presentation().published().unwrap().revision(), 0);

        let mut replacement = MemoryIo::default();
        replacement.incoming.push_back(snapshot(1));
        driver.replace_authenticated_attachment(replacement);
        assert!(matches!(
            block_on(driver.receive_once()),
            Ok(LiveSessionEvent::SnapshotPublished { revision: 1 })
        ));
        assert_eq!(driver.renderer.revisions, vec![1]);
    }
}
