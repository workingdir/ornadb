#[path = "../src/repl.rs"]
mod repl;

use std::collections::VecDeque;

use orna_client::live_presentation::{ResyncRequest, WatchPresentation};
use orna_conformance_v1::AdmittedReplSession;
use orna_evaluator_v1::Limits;
use repl::{WatchCommandState, WatchFrameSource};

const WATCH: [u8; 16] = [7; 16];
const WATCH_EXPRESSIONS: &str = include_str!("fixtures/watch-command-expressions.orna");

fn expression(index: usize) -> &'static str {
    WATCH_EXPRESSIONS
        .lines()
        .nth(index)
        .expect("watch expression fixture line")
}

#[derive(Default)]
struct FrameSource {
    subscriptions: Vec<String>,
    watch_ids: VecDeque<[u8; 16]>,
    frames: VecDeque<VecDeque<Vec<u8>>>,
    active_frames: VecDeque<Vec<u8>>,
    active_watch: [u8; 16],
    resync_attempts: Vec<ResyncRequest>,
    resyncs: Vec<ResyncRequest>,
    failed_resyncs_remaining: usize,
    fail_closes: bool,
    closed_watches: Vec<[u8; 16]>,
}

impl WatchFrameSource for FrameSource {
    type Error = &'static str;

    fn start_watch(&mut self, source: &str) -> Result<WatchPresentation, Self::Error> {
        self.subscriptions.push(source.to_owned());
        self.active_watch = self.watch_ids.pop_front().unwrap_or(WATCH);
        self.active_frames = self.frames.pop_front().unwrap_or_default();
        WatchPresentation::new(self.active_watch, Default::default())
            .map_err(|_| "invalid negotiated limits")
    }

    fn next_frame(&mut self, watch: [u8; 16]) -> Result<Option<Vec<u8>>, Self::Error> {
        assert_eq!(
            watch, self.active_watch,
            "frames belong to the subscribed watch"
        );
        Ok(self.active_frames.pop_front())
    }

    fn request_resync(&mut self, request: ResyncRequest) -> Result<(), Self::Error> {
        assert_eq!(
            request.watch(),
            self.active_watch,
            "resync belongs to the subscribed watch"
        );
        self.resync_attempts.push(request);
        if self.failed_resyncs_remaining > 0 {
            self.failed_resyncs_remaining -= 1;
            return Err("temporary resync failure");
        }
        self.resyncs.push(request);
        Ok(())
    }

    fn close_watch(&mut self, watch: [u8; 16]) -> Result<(), Self::Error> {
        self.closed_watches.push(watch);
        if self.fail_closes {
            Err("temporary close failure")
        } else {
            Ok(())
        }
    }
}

// These are complete canonical protocol frames, not only the body of a message.
// The layouts mirror the live presentation consumer's Snapshot and Delta tests.
fn envelope_for(watch: [u8; 16], code: u8, body: Vec<u8>) -> Vec<u8> {
    let mut bytes = vec![0xa5, 0x00, 0x01, 0x01, code, 0x02, 0xf6, 0x03, 0x50];
    bytes.extend(watch);
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

fn snapshot_for(watch: [u8; 16], revision: u8, value: &str) -> Vec<u8> {
    // Present: tag 60012, text node, null key, {text: value}, no children.
    let mut body = vec![0xa3, 0x00, revision, 0x01, 0xd9, 0xea, 0x6c, 0x84, 0x64];
    body.extend(b"text");
    body.extend([0xf6, 0xa1, 0x64]);
    body.extend(b"text");
    body.extend(text(value));
    body.extend([0x80, 0x02]);
    body.extend(pinned_snapshot());
    envelope_for(watch, 16, body)
}

fn snapshot(revision: u8, value: &str) -> Vec<u8> {
    snapshot_for(WATCH, revision, value)
}

fn delta_for(watch: [u8; 16], base: u8, revision: u8, value: &str) -> Vec<u8> {
    // One Replace at the root's text property: [2, [[0, "text"]], value].
    let mut body = vec![
        0xa4, 0x00, base, 0x01, revision, 0x02, 0x81, 0x83, 0x02, 0x81, 0x82, 0x00, 0x64,
    ];
    body.extend(b"text");
    body.extend(text(value));
    body.push(0x03);
    body.extend(pinned_snapshot());
    envelope_for(watch, 17, body)
}

fn delta(base: u8, revision: u8, value: &str) -> Vec<u8> {
    delta_for(WATCH, base, revision, value)
}

impl FrameSource {
    fn queue_frames(&mut self, frames: impl IntoIterator<Item = Vec<u8>>) {
        self.frames.push_back(frames.into_iter().collect());
    }
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
    let expression = include_str!("fixtures/unwired-watch-expression.orna").trim();
    let input = format!(":watch {expression}\n{expression}\n:quit\n");
    let mut input = input.as_bytes();
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
    source.queue_frames([snapshot(1, "one"), delta(1, 2, "two")]);
    let mut state = WatchCommandState::new();
    let watch_expression = expression(0);
    let submitted_expression = expression(1);
    let input = format!(":watch {watch_expression}\n{submitted_expression}\n:quit\n");
    let output = run(&input, &mut source, &mut state);
    assert_eq!(source.subscriptions, [watch_expression]);
    assert!(
        output.contains("watch: snapshot installed (rev 1)"),
        "{output}"
    );
    assert!(output.contains("watch: delta applied (rev 2)"), "{output}");
    assert!(output.contains("7 : Int"), "{output}");
    assert_eq!(source.closed_watches, [WATCH]);
    assert_eq!(state.watch(), None);
}

#[test]
fn mismatched_delta_requests_shared_resync_until_complete_snapshot_recovers() {
    let mut source = FrameSource::default();
    source.queue_frames([
        snapshot(1, "one"),
        delta(9, 10, "wrong"),
        snapshot(11, "fresh"),
    ]);
    source.failed_resyncs_remaining = 1;
    let mut state = WatchCommandState::new();
    let input = format!(":watch {}\n:quit\n", expression(0));
    let output = run(&input, &mut source, &mut state);
    assert!(output.contains("watch: resync required"), "{output}");
    assert!(output.contains("error[ORNA-REPL-WATCH-SOURCE]"), "{output}");
    assert!(
        output.contains("watch: snapshot installed (rev 11)"),
        "{output}"
    );
    assert!(!output.contains("watch: delta applied"), "{output}");
    assert_eq!(source.resync_attempts.len(), 2);
    assert_eq!(source.resyncs.len(), 1);
    assert_eq!(source.resyncs[0].watch(), WATCH);
    assert_eq!(source.closed_watches, [WATCH]);
}

#[test]
fn reconnect_fences_old_deltas_and_installs_replacement_snapshot() {
    let replacement = [8; 16];
    let mut source = FrameSource::default();
    source.watch_ids.extend([WATCH, replacement]);
    source.queue_frames([snapshot(3, "before")]);
    let mut state = WatchCommandState::new();
    let input = format!(":watch {}\n:quit\n", expression(0));
    let output = run(&input, &mut source, &mut state);
    assert!(
        output.contains("watch: snapshot installed (rev 3)"),
        "{output}"
    );
    assert_eq!(state.watch(), None, "closing the REPL releases its watch");
    source.queue_frames([
        delta(3, 4, "old"),
        snapshot_for(replacement, 8, "replacement"),
    ]);
    let input = format!(":watch {}\n:quit\n", expression(0));
    let output = run(&input, &mut source, &mut state);
    assert!(!output.contains("watch: delta applied"), "{output}");
    assert!(
        output.contains("watch: snapshot installed (rev 8)"),
        "{output}"
    );
    assert_eq!(source.subscriptions, [expression(0), expression(0)]);
    assert!(source.resyncs.is_empty());
    assert_eq!(source.closed_watches, [WATCH, replacement]);
}

#[test]
fn end_of_input_closes_the_session_watch() {
    let mut source = FrameSource::default();
    let mut state = WatchCommandState::new();
    let input = format!(":watch {}\n", expression(0));
    let _output = run(&input, &mut source, &mut state);
    assert_eq!(source.closed_watches, [WATCH]);
    assert_eq!(state.watch(), None);
}

#[test]
fn replacing_a_watch_closes_the_previous_owner_and_fences_its_frames() {
    let replacement = [8; 16];
    let mut source = FrameSource::default();
    source.watch_ids.extend([WATCH, replacement]);
    source.queue_frames([snapshot(1, "old")]);
    source.queue_frames([
        delta_for(WATCH, 1, 2, "stale"),
        snapshot_for(replacement, 8, "replacement"),
    ]);
    let mut state = WatchCommandState::new();
    let expression = expression(0);
    let input = format!(":watch {expression}\n:watch {expression}\n:quit\n");
    let output = run(&input, &mut source, &mut state);

    assert!(output.contains("error[ORNA-REPL-WATCH-FRAME]"), "{output}");
    assert!(
        output.contains("watch: snapshot installed (rev 8)"),
        "{output}"
    );
    assert!(!output.contains("watch: delta applied"), "{output}");
    assert_eq!(source.closed_watches, [WATCH, replacement]);
    assert_eq!(state.watch(), None);
}

#[test]
fn failed_host_close_is_reported_during_graceful_session_close() {
    let mut source = FrameSource {
        fail_closes: true,
        ..FrameSource::default()
    };
    let mut state = WatchCommandState::new();
    let input = format!(":watch {}\n:quit\n", expression(0));
    let output = run(&input, &mut source, &mut state);

    assert!(output.contains("error[ORNA-REPL-WATCH-SOURCE]"), "{output}");
    assert_eq!(source.closed_watches, [WATCH]);
    assert_eq!(state.watch(), None);
}
