# Astra v1 client-watch review

## Scope and authority

This review pins the requested baseline at `9cd3b76de653bddcb8d9433ec5d70f1c125b5260` and compares it with current main at `8183e753349b15f09b5693e1c923af19c90cbea6`. Scope is the live client reconnect/watch boundary; HTTP transport and authentication internals were not reviewed.

Verified frozen-reference authority:

- **ORNA-WIRE-001** (`source/14-pages.md:46`): every watch begins with a complete snapshot.
- **ORNA-WIRE-002** (`source/14-pages.md:48`): watch identity and revision are stable per watch; deltas apply against their declared base.
- **ORNA-WIRE-006** (`source/14-pages.md:56`): reconnect resubscribes and receives a complete current snapshot.
- **ORNA-PROTO-002** (`source/30-protocol.md:23`): session resume preserves request reservations and terminal outcomes.

## Findings

### Historical baseline failure: a fresh watch inherited the old revision

**Disposition: confirmed at the pinned baseline; fixed on the current branch of development.** At baseline, `LiveSessionDriver::replace_authenticated_attachment_with_watch` copied the old published snapshot and revision into the new watch before starting resubscription (`crates/orna-client/src/live_session.rs:194-217`). A complete revision-zero snapshot for the new watch was then treated as a rollback and caused `ResyncSent`, contrary to the fresh-watch snapshot requirement.

An untracked overlay of the current `v1_watch_review_regressions` target was run against the pinned baseline. Four controls passed; both cases that vary the fresh watch's zero revision failed:

```text
running 6 tests
test fresh_watch_a_rev5_to_b_rev0_accepts_snapshot_and_delta ... FAILED
test fresh_watch_a0_to_changed_b0_accepts_snapshot_and_delta ... FAILED
test result: FAILED. 4 passed; 2 failed; 0 ignored
exit code: 101
```

Both failures returned `ResyncSent` where `SnapshotPublished { revision: 0 }` was expected. Commit `4de8029f27bad62ef8c92ea9b5912fb4a5f510c7` removed the old-tree copy when replacing a watch. The same review target on current main passes all six cases:

```text
running 6 tests
test fresh_watch_a_rev5_to_b_rev0_accepts_snapshot_and_delta ... ok
test fresh_watch_a0_to_changed_b0_accepts_snapshot_and_delta ... ok
test result: ok. 6 passed; 0 failed; 0 ignored
exit code: 0
```

No additional implementation follow-up is needed for this finding; it is already covered by the existing #790 fix and regression target.

### Reused Subscribe request identity makes the #772 reconnect fixture unrealistic

**Disposition: confirmed test-fidelity gap with a client reconnect consequence.** `crates/orna-client/tests/live_reconnect.rs:86-102` hard-codes Subscribe request ID `[1; 16]`; `initial_driver` sends that request for the first watch (`:421-431`) and reconnect tests call the same `subscribe()` again (`:447-449`, `:510-512`). The loopback fixture checks that bytes match and fabricates a different watch snapshot (`:284-295`), but does not model the resumed session's retained request ledger.

At baseline, the host keys request records by `(session, request)` and returns the prior terminal record when the fingerprint matches (`crates/orna-live-v1/src/lib.rs:2105-2112`). Thus the identical Subscribe on the same resumed session can replay the old snapshot/watch instead of creating a fresh watch. `BootstrappedLiveAttachment::replace_driver` rejects a returned watch equal to the current watch (`crates/orna-client/src/live_bootstrap.rs:206-223`), so the public reconnect fails with `WatchIdentity` and returns the rotated session through `LiveReconnectError::AfterResume`. The mock's fabricated new watch hides this outcome.

This remains outstanding on current main: the loopback fixture still uses the fixed ID, and the retained-request fast path still returns the prior terminal outcome (`crates/orna-client/tests/live_reconnect.rs:100-103,421-432,435-452`; `crates/orna-live-v1/src/lib.rs:2995-3005`).

The newer `v1_watch_review_regressions` target uses distinct request IDs for A and B (`crates/orna-client/tests/v1_watch_review_regressions.rs:33-36,258-306`), but exercises only the public in-memory client boundary and does not validate host ledger replay. Durable follow-up: **ornadb-gov5.17.2.1 / GitHub #1965 — Reconcile client reconnect Subscribe request identity**. This review does not choose whether the client allocates a fresh ID or rejects reused IDs; it records the need for a distinct logical resubscription and a host-retention-aware test.

### Dropping reconnect after credential rotation can lose the replacement session

**Disposition: confirmed API availability risk; normative disposition is no-delta because the frozen reference does not specify this cancellation boundary.** `LiveClient::reconnect_driver` and `resume_driver` store the result of `resume_session` in a local `replacement`, then await connection and bootstrap (`crates/orna-client/src/live_bootstrap.rs:245-286,313-362`). If the caller drops either future during a later await, Rust drops that local `LiveSession`; the caller still owns only its original borrowed session. The server has already rotated credentials, and explicit `AfterResume` error paths that return the replacement are bypassed. The session cannot be retried with the credential that the cancelled future discarded.

The same local-replacement-across-await structure remains on current main (`crates/orna-client/src/live_bootstrap.rs:313-362,384-438`; credential rotation returns from `crates/orna-client/src/live_transport.rs:175-197`).

ORNA-PROTO-002 specifies preservation of request reservations and terminal outcomes across resume, but does not prescribe client-future cancellation or credential handoff behavior. No protocol behavior is invented here. Durable follow-up: **ornadb-gov5.17.2.2 / GitHub #1964 — Define cancellation-safe client resume credential handoff**. The API contract must be decided before selecting an implementation; deterministic cancellation coverage should follow that decision.

## Evidence limits

The regression target was run at the exact pinned baseline using a temporary untracked test overlay, and against current main. Request replay and future-drop findings are source-level counterexamples; this review did not run an end-to-end host/client test for them. No Rust tests contain Orna source text, and no `.orna` fixture is applicable.
