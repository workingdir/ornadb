#[path = "../src/repl.rs"]
mod repl;

use std::collections::VecDeque;

use orna_client::live_presentation::WatchPresentation;
use orna_conformance_v1::AdmittedReplSession;
use orna_evaluator_v1::Limits;
use repl::{WatchCommandState, WatchFrameSource};

const WATCH: [u8; 16] = [7; 16];

#[derive(Default)]
struct FrameSource {
    subscriptions: Vec<String>,
    frames: VecDeque<Vec<u8>>,
}

impl WatchFrameSource for FrameSource {
    type Error = &'static str;

    fn start_watch(&mut self, source: &str) -> Result<WatchPresentation, Self::Error> {
        self.subscriptions.push(source.to_owned());
        WatchPresentation::new(WATCH, Default::default()).map_err(|_| "invalid negotiated limits")
    }

    fn next_frame(&mut self, watch: [u8; 16]) -> Result<Option<Vec<u8>>, Self::Error> {
        assert_eq!(watch, WATCH, "frames belong to the subscribed watch");
        Ok(self.frames.pop_front())
    }
}

// These are complete canonical protocol frames, not only the body of a message.
// The layouts mirror the live presentation consumer's Snapshot and Delta tests.
fn envelope(code: u8, body: Vec<u8>) -> Vec<u8> {
    let mut bytes = vec![0xa5, 0x00, 0x01, 0x01, code, 0x02, 0xf6, 0x03, 0x50];
    bytes.extend(WATCH);
    bytes.push(0x04);
    bytes.extend(body);
    bytes
}

fn text(value: &str) -> Vec<u8> {
    assert!(value.len() < 24);
    let mut bytes = vec![0x60 + value.len() as u8];
    bytes.extend(value.as_bytes());
    bytes
}

fn pinned_snapshot() -> Vec<u8> {
    let mut bytes = vec![0x84, 0x01, 0xd8, 0x25, 0x50];
    bytes.extend([1; 16]);
    bytes.extend([0x66]);
    bytes.extend(b"sha256");
    bytes.extend([0x58, 0x20]);
    bytes.extend([2; 32]);
    bytes
}

fn snapshot(revision: u8, value: &str) -> Vec<u8> {
    // Present: tag 60012, text node, null key, {text: value}, no children.
    let mut body = vec![0xa3, 0x00, revision, 0x01, 0xd9, 0xea, 0x6c, 0x84, 0x64];
    body.extend(b"text");
    body.extend([0xf6, 0xa1, 0x64]);
    body.extend(b"text");
    body.extend(text(value));
    body.extend([0x80, 0x02]);
    body.extend(pinned_snapshot());
    envelope(16, body)
}

fn delta(base: u8, revision: u8, value: &str) -> Vec<u8> {
    // One Replace at the root's text property: [2, [[0, "text"]], value].
    let mut body = vec![
        0xa4, 0x00, base, 0x01, revision, 0x02, 0x81, 0x83, 0x02, 0x81, 0x82, 0x00, 0x64,
    ];
    body.extend(b"text");
    body.extend(text(value));
    body.push(0x03);
    body.extend(pinned_snapshot());
    envelope(17, body)
}

fn run(input: &str, source: &mut FrameSource, state: &mut WatchCommandState) -> String {
    let mut session = AdmittedReplSession::new(Limits::default());
    let mut output = Vec::new();
    repl::run_with_watch(
        &mut input.as_bytes(),
        &mut output,
        &mut session,
        source,
        state,
    )
    .expect("REPL remains interactive");
    String::from_utf8(output).expect("REPL output is UTF-8")
}

#[test]
fn unwired_watch_reports_command_error_and_retains_expression_execution() {
    let mut input = b":watch 1 + 1\n1 + 1\n:quit\n".as_slice();
    let mut output = Vec::new();
    let mut session = AdmittedReplSession::new(Limits::default());
    repl::run(&mut input, &mut output, &mut session).expect("REPL remains interactive");
    let output = String::from_utf8(output).unwrap();
    assert!(output.contains("error[ORNA-REPL-COMMAND]"), "{output}");
    assert!(output.contains("2 : Int"), "{output}");
}

#[test]
fn watch_installs_snapshot_and_applies_ordered_delta_without_disrupting_expressions() {
    let mut source = FrameSource::default();
    source
        .frames
        .extend([snapshot(1, "one"), delta(1, 2, "two")]);
    let mut state = WatchCommandState::new();
    let output = run(":watch 1 + 1\n3 + 4\n:quit\n", &mut source, &mut state);
    assert_eq!(source.subscriptions, ["1 + 1"]);
    assert!(
        output.contains("watch: snapshot installed (rev 1)"),
        "{output}"
    );
    assert!(output.contains("watch: delta applied (rev 2)"), "{output}");
    assert!(output.contains("7 : Int"), "{output}");
    assert_eq!(state.published_revision(), Some(2));
    assert_eq!(state.watch(), Some(WATCH));
    assert!(!state.awaiting_snapshot());
}

#[test]
fn mismatched_delta_keeps_published_revision_until_complete_snapshot_recovers() {
    let mut source = FrameSource::default();
    source
        .frames
        .extend([snapshot(1, "one"), delta(9, 10, "wrong")]);
    let mut state = WatchCommandState::new();
    let output = run(":watch 1 + 1\n:quit\n", &mut source, &mut state);
    assert!(output.contains("watch: resync required"), "{output}");
    assert_eq!(state.published_revision(), Some(1));
    assert!(state.awaiting_snapshot());
    let request = state
        .take_resync_request()
        .expect("resync remains pending until sent");
    assert_eq!(request.watch(), WATCH);
    state.acknowledge_resync_request(request);
    assert_eq!(state.take_resync_request(), None);
    assert!(
        state.awaiting_snapshot(),
        "send does not remove snapshot barrier"
    );

    source
        .frames
        .extend([delta(1, 2, "stale"), snapshot(11, "fresh")]);
    let output = run("1 + 1\n:quit\n", &mut source, &mut state);
    assert!(
        output.contains("watch: snapshot installed (rev 11)"),
        "{output}"
    );
    assert!(!output.contains("watch: delta applied"), "{output}");
    assert_eq!(state.published_revision(), Some(11));
    assert!(!state.awaiting_snapshot());
}

#[test]
fn reconnect_fences_old_deltas_and_installs_replacement_snapshot() {
    let mut source = FrameSource::default();
    source.frames.push_back(snapshot(3, "before"));
    let mut state = WatchCommandState::new();
    let output = run(":watch 1 + 1\n:quit\n", &mut source, &mut state);
    assert!(
        output.contains("watch: snapshot installed (rev 3)"),
        "{output}"
    );
    state.begin_resubscription();
    assert_eq!(state.published_revision(), Some(3));
    assert!(state.awaiting_snapshot());
    source
        .frames
        .extend([delta(3, 4, "old"), snapshot(8, "replacement")]);
    let output = run("1 + 1\n:quit\n", &mut source, &mut state);
    assert!(!output.contains("watch: delta applied"), "{output}");
    assert!(
        output.contains("watch: snapshot installed (rev 8)"),
        "{output}"
    );
    assert_eq!(state.published_revision(), Some(8));
    assert!(!state.awaiting_snapshot());
    assert_eq!(source.subscriptions, ["1 + 1"]);
}
