# Astra v1 server-cancel review — 2026-09-29

**Scope:** GitHub #789 / Beads `ornadb-gov5.17.3`; independent review of server application cancellation and WebSocket control handling. Current source base: `8183e753349b15f09b5693e1c923af19c90cbea6` (`origin/main`). Review only; no production source or test changes.

## Normative basis

- `source/10-execution.md:42`, **ORNA-CONCUR-001**: when an owner is cancelled or lost, request cancellation of unfinished owned work and join termination before marking the owner terminal.
- `source/10-execution.md:63-71`, TASK-END-1 steps 1, 3, 4, and 7: stop child admission, cancel children, join termination and release resources, then publish terminal state and release the owner lease. Step 4 requires cancellation checks at bounded VM, loop, stream, and host-call boundaries.
- `source/10-execution.md:75`, **ORNA-TASK-002**: graceful REPL close cancels session-owned invocations and watches without terminating unrelated sessions or runs.
- `source/30-protocol.md:5`: ping, pong, and close retain their WebSocket meanings.
- `source/30-protocol.md:19`: loss of the WebSocket starts the advertised finite reconnection lease; no new client request is admitted on a lost connection.
- `source/30-protocol.md:51, 61`: the client can send a Cancel request; its acknowledgement is separate from the target's terminal result.

## Finding: a pending application callback prevents its WebSocket from processing control input

**Evidence category:** current production call-flow inspection; static counterexample. The precise proposed end-to-end regression has not been executed, so this finding is not presented as a failing test.

In `crates/orna-live-v1/src/lib.rs`, `serve_websocket_connection` reads socket bytes at lines 6667-6688 and then awaits `serve_websocket_bytes` at lines 6689-6699. `serve_websocket_bytes` calls `receive_one_with_application` and awaits it at lines 7327-7333. For a Binary frame, `receive_one_with_application` awaits `host.dispatch_frame` at lines 7199-7209. The Ping, Pong, and Close arms are reached only after that dispatch returns (lines 7233-7243). Subsequent socket reads and EOF handling are outside this awaited call (lines 6668-6687).

`LiveApplication::dispatch_with_work` is explicitly allowed to await `LiveApplicationWorkLease::cancellation()` at bounded checkpoints (`lib.rs:1119-1133`). Consider a legal cancellation-aware application callback that waits for that cancellation notification. Once the peer's Binary request enters `dispatch_frame`, the serving future remains in the awaited application callback. It cannot read a subsequent Cancel request or Ping, and cannot observe peer EOF. The callback waits for cancellation that this same socket loop cannot deliver. This is a concrete liveness counterexample from the production control flow; it does not claim every application callback blocks or that unrelated sessions stop progressing.

The existing tests establish narrower pieces, not this interleaving. `tests/v1_review_regressions.rs:398-425` tests Close and Ping through `prepare_websocket_application`, and `:87-134` tests cancellation/drain at the supervisor. The in-band request-cancel unit test is at `src/lib.rs:8269-8318`. None drives `serve_websocket_connection` with a gated pending application callback and a second control frame/EOF.

**Durable follow-up:** add a deterministic socket-loop regression using a gated `LiveApplication` whose Eval dispatch waits on its work lease's cancellation notification. Through the executable WebSocket connection path, hold that callback pending and prove that an in-band Cancel, Ping, and peer write-half shutdown are each serviced before releasing the gate. Use explicit poll/waker barriers, not sleeps or elapsed-time thresholds. Then implement a serving design that can observe control/read lifecycle while application work is pending and preserve the required cancellation/join ordering. This report does not prescribe an unreferenced wire behavior or make the production change.

## Baseline findings reconciled against current main

The review notes at baseline `9cd3b76d` also described request-tree cancellation identity and global session draining as gaps. Current main contains `e1d00f0e` (`fix(live): join cancelled request trees`, #789) and `f1ffd044` (`fix(live): drain every session after cancellation broadcast`, #795). Current code retains request identity on each work lease (`src/lib.rs:218-243`) and cancels/joins the targeted request tree (`:342-397`); `cancel_and_join_all` begins draining every session before joining them and retains an error until all joins finish (`:569-597`). The coalesced-frame and peer-Close paths are also handled by the current complete-frame loop and close-output handling (`:7143-7171`, `:7238-7243`, `:7327-7361`). These are not reported as current gaps.

## Focused CLI evidence

All commands ran from the clean review worktree against the frozen reference sibling. Output below is the focused test summary; unrelated existing unused-import/unused-mut compiler warnings appeared in `orna-repository-v1`.

```text
$ cargo test --locked --offline -p orna-live-v1 --test v1_review_regressions -- --nocapture
running 6 tests
... 6 passed; 0 failed; 0 ignored
exit: 0

$ cargo test --locked --offline -p orna-live-v1 --lib application_work_ -- --nocapture
running 4 tests
... 4 passed; 0 failed; 0 ignored; 31 filtered out
exit: 0

$ cargo test --locked --offline -p orna-live-v1 --test live_host failed_child_join_after_durable_cancellation_never_reports_delete_success -- --exact --nocapture
running 1 test
... 1 passed; 0 failed; 0 ignored; 111 filtered out
exit: 0
```

These passing tests support only the supervisor, cancellation, and protocol-preparation behavior they exercise. They do not close the socket-loop liveness finding above. No `.orna` fixture applies because this review adds no Orna source input.
