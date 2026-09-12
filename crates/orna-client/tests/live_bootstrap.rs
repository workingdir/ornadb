use orna_client::{
    AuthenticatedLiveTransport, BootstrappedLiveAttachment, LiveByteDriver, LiveSessionEvent,
    PresentRenderer, RequestIdAllocator,
};
use orna_protocol_v1::{
    CanonicalSnapshot, Envelope, Limits, Message, PresentNode, PresentationContext,
};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    collections::VecDeque,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

struct FakeBinaryTransport {
    incoming: VecDeque<Vec<u8>>,
    sent: Rc<RefCell<Vec<Vec<u8>>>>,
    reads: Rc<RefCell<usize>>,
}

impl LiveByteDriver for FakeBinaryTransport {
    type Error = &'static str;

    fn receive_binary<'a>(
        &'a mut self,
        _max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, Self::Error>> + 'a>> {
        *self.reads.borrow_mut() += 1;
        Box::pin(async move { self.incoming.pop_front().ok_or("no message") })
    }

    fn send_binary<'a>(
        &'a mut self,
        bytes: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + 'a>> {
        Box::pin(async move {
            self.sent.borrow_mut().push(bytes);
            Ok(())
        })
    }
}

impl AuthenticatedLiveTransport for FakeBinaryTransport {}

#[derive(Default)]
struct Renderer {
    revisions: Vec<u64>,
}

impl PresentRenderer for Renderer {
    type Error = &'static str;

    fn publish(
        &mut self,
        _watch: [u8; 16],
        revision: u64,
        _snapshot: &CanonicalSnapshot,
        _present: &PresentNode,
    ) -> Result<(), Self::Error> {
        self.revisions.push(revision);
        Ok(())
    }
}

#[derive(Default)]
struct Requests;

impl RequestIdAllocator for Requests {
    fn next_request_id(&mut self) -> [u8; 16] {
        [9; 16]
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

fn subscribe() -> Envelope {
    Envelope {
        request: Some([1; 16]),
        watch: None,
        message: Message::Subscribe {
            resource: [2; 16],
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

fn snapshot(watch: [u8; 16], request: [u8; 16]) -> Vec<u8> {
    let mut bytes = vec![0xa5, 0x00, 0x01, 0x01, 16, 0x02, 0x50];
    bytes.extend(request);
    bytes.extend([0x03, 0x50]);
    bytes.extend(watch);
    bytes.extend([0x04, 0xa3, 0x00, 0x00, 0x01]);
    bytes.extend([0xd9, 0xea, 0x6c, 0x84, 0x64]);
    bytes.extend(b"text");
    bytes.extend([0xf6, 0xa1, 0x64]);
    bytes.extend(b"text");
    bytes.extend([0x62]);
    bytes.extend(b"ok");
    bytes.extend([0x80, 0x02, 0x84, 0x01, 0xd8, 0x25, 0x50]);
    bytes.extend([1; 16]);
    bytes.extend([0x66]);
    bytes.extend(b"sha256");
    bytes.extend([0x58, 32]);
    bytes.extend([2; 32]);
    Envelope::decode(&bytes, Limits::default())
        .unwrap()
        .encode(Limits::default())
        .unwrap()
}

#[test]
fn bootstrap_sends_subscribe_and_transfers_first_snapshot_once() {
    let request = subscribe();
    let first = snapshot([7; 16], [1; 16]);
    let sent = Rc::new(RefCell::new(Vec::new()));
    let reads = Rc::new(RefCell::new(0));
    let transport = FakeBinaryTransport {
        incoming: VecDeque::from([first.clone()]),
        sent: Rc::clone(&sent),
        reads: Rc::clone(&reads),
    };
    let attachment = block_on(BootstrappedLiveAttachment::start(
        transport,
        request.clone(),
        Limits::default(),
    ))
    .unwrap();
    assert_eq!(
        sent.borrow().as_slice(),
        &[request.encode(Limits::default()).unwrap()]
    );
    assert_eq!(attachment.watch(), [7; 16]);
    let mut driver = attachment
        .into_driver(Renderer::default(), Requests)
        .unwrap();
    assert!(matches!(
        block_on(driver.receive_once()),
        Ok(LiveSessionEvent::SnapshotPublished { revision: 0 })
    ));
    assert_eq!(
        *reads.borrow(),
        1,
        "bootstrap performed the only read for the preserved snapshot"
    );
    assert_eq!(driver.presentation().published().unwrap().revision(), 0);
    assert!(matches!(
        block_on(driver.receive_once()),
        Err(orna_client::LiveSessionError::Io("no message"))
    ));
    assert_eq!(
        *reads.borrow(),
        2,
        "the second driver step reads the underlying transport"
    );
}

#[test]
fn bootstrap_rejects_non_subscribe_without_writing() {
    let request = Envelope {
        request: Some([1; 16]),
        watch: None,
        message: Message::Resync,
        extensions: BTreeMap::new(),
    };
    let result = block_on(BootstrappedLiveAttachment::start(
        FakeBinaryTransport {
            incoming: VecDeque::new(),
            sent: Rc::new(RefCell::new(Vec::new())),
            reads: Rc::new(RefCell::new(0)),
        },
        request,
        Limits::default(),
    ));
    assert!(matches!(
        result,
        Err(orna_client::LiveBootstrapError::InvalidRequest)
    ));
}

#[test]
fn bootstrap_rejects_snapshot_for_a_different_request() {
    let transport = FakeBinaryTransport {
        incoming: VecDeque::from([snapshot([7; 16], [9; 16])]),
        sent: Rc::new(RefCell::new(Vec::new())),
        reads: Rc::new(RefCell::new(0)),
    };
    let result = block_on(BootstrappedLiveAttachment::start(
        transport,
        subscribe(),
        Limits::default(),
    ));
    assert!(matches!(
        result,
        Err(orna_client::LiveBootstrapError::UnexpectedResponse)
    ));
}
