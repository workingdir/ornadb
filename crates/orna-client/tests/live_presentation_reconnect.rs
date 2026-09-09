use orna_client::live_presentation::{LivePresentationUpdate, WatchPresentation};
use orna_protocol_v1::{Envelope, Limits};

fn frame(code: u8, watch: [u8; 16], body: Vec<u8>) -> Envelope {
    let mut bytes = vec![0xa5, 0x00, 0x01, 0x01, code, 0x02, 0xf6, 0x03, 0x50];
    bytes.extend(watch);
    bytes.push(0x04);
    bytes.extend(body);
    Envelope::decode(&bytes, Limits::default()).expect("canonical presentation frame")
}

fn snapshot(revision: u8, text: &str) -> Vec<u8> {
    let mut body = vec![0xa3, 0x00, revision, 0x01];
    body.extend(present(text));
    body.push(0x02);
    body.extend(pinned_snapshot());
    body
}

fn delta(base: u8, next: u8, text: &str) -> Vec<u8> {
    let mut body = vec![
        0xa4, 0x00, base, 0x01, next, 0x02, 0x81, 0x83, 0x02, 0x81, 0x82,
    ];
    body.extend([0x00]);
    body.extend(cbor_text("text"));
    body.extend(cbor_text(text));
    body.push(0x03);
    body.extend(pinned_snapshot());
    body
}

fn present(text: &str) -> Vec<u8> {
    let mut node = vec![0xd9, 0xea, 0x6c, 0x84, 0x64];
    node.extend(b"text");
    node.extend([0xf6, 0xa1]);
    node.extend(cbor_text("text"));
    node.extend(cbor_text(text));
    node.push(0x80);
    node
}

fn cbor_text(text: &str) -> Vec<u8> {
    assert!(text.len() < 24);
    let mut value = vec![0x60 + text.len() as u8];
    value.extend(text.as_bytes());
    value
}

fn pinned_snapshot() -> Vec<u8> {
    let mut snapshot = vec![0x84, 0x01, 0xd8, 0x25, 0x50];
    snapshot.extend([1; 16]);
    snapshot.extend([0x66]);
    snapshot.extend(b"sha256");
    snapshot.extend([0x58, 32]);
    snapshot.extend([2; 32]);
    snapshot
}

#[test]
fn resubscription_fences_matching_stale_deltas_until_a_complete_snapshot() {
    let watch = [7; 16];
    let mut state = WatchPresentation::new(watch, Limits::default()).expect("valid limits");
    assert_eq!(
        state.receive(&frame(16, watch, snapshot(4, "before"))),
        Ok(LivePresentationUpdate::SnapshotInstalled)
    );
    let before = state.published().cloned();

    state.begin_resubscription();
    assert!(state.awaiting_snapshot());
    assert_eq!(
        state.receive(&frame(17, watch, delta(4, 5, "stale"))),
        Ok(LivePresentationUpdate::ResyncRequired)
    );
    assert_eq!(state.published().cloned(), before);

    assert_eq!(
        state.receive(&frame(16, watch, snapshot(5, "current"))),
        Ok(LivePresentationUpdate::SnapshotInstalled)
    );
    assert_eq!(
        state
            .published()
            .expect("recovered presentation")
            .revision(),
        5
    );
    assert!(!state.awaiting_snapshot());
}
