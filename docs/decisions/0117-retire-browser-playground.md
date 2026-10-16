# Work ADR 0117: Retire the Browser Playground

**Status:** Accepted (D6; supersedes work ADR 0116)

## Decision

Remove the browser playground as a product: its database-hosted application,
HTTP routes and assets, browser/WASM evaluator path, browser-only LSP bridge,
playground editor metadata, build workflow, and app-specific fixtures are not
part of OrnaDB 1.1.0. No replacement browser application is introduced by this
decision.

Keep the native `orna-cli-v1` integration suite as an independent CI gate. Its
workflow uses per-ref concurrency and does not cancel an in-progress run, so a
worker ref cannot replace the main ref's pending CLI gate.

Keep generic native `orna serve`, Git transport, and the ordinary removable
`std.devtools` frontend. The frontend remains table-first and exposes the
spec-required database views. This does not add a second language execution
path or a playground-specific route.

## Normative boundary

The Orna 1.1.0 reference remains unchanged and authoritative. This decision
removes implementation surfaces not required by the generic serving contract;
it does not remove or weaken that contract:

* `ORNA-SERVE-001` keeps Git transport, page/query endpoints, and WebSocket
  presentation deltas available from `orna serve`.
* `ORNA-SERVE-007` keeps Git transport usable when the optional web frontend
  is absent.
* `ORNA-SERVE-008` permits an ordinary, removable devtools application.
* `ORNA-SERVE-009` keeps the default frontend table-first and exposes files,
  commits, branches, functions, dependencies, storage, and runtime state.

## Consequences

Native CLI tests run independently of retired browser assets and browser-only
LSP exports. Generic editor grammar generation, native LSP functionality, and
generic serving/devtools remain governed by their existing contracts. The
historical work ADR 0116 records its original rationale only; this decision
supersedes its application, route, runtime, and asset commitments.