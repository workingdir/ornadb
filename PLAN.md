# OrnaDB remaining implementation plan

## Status and scope

This plan covers the remaining roadmap surfaces after the accepted `orna.std/5`
JSON value slice and the CLIENT resource and USER state slices.

The plan is deliberately dormant until the canonical specification accepts the
missing executable contracts. The current specification marks the graphical
runtime ABI, UI event boundary, resource inspection projection, and reflective
gateway details as `CURRENT PROPOSAL` or `OPEN`. The repository must not treat
those drafts as implementation contracts.

Current accepted implementation evidence:

- `TODO.md:54-80` records the completed CLIENT, TTY, Inspector-core, source,
  security, presenter, state, resource, identity, transport, and JSON slices.
- `docs/decisions/0062-std-ui-value-type.md` accepts the transient `std.ui.UI`
  value only. It defers the graphical runtime ABI and UI sink.
- `docs/decisions/0063-automatic-runtime-selection.md` accepts the TTY offer and
  deterministic selection only. Non-TTY runtimes remain later work.
- `docs/decisions/0064-sys-inspect-core.md` accepts server-side snapshots and
  the `orna inspect` CLI. UI nodes and resource projections remain empty by
  contract.
- `docs/decisions/0068-client-expression-bodies.md` accepts closed CLIENT
  expressions but defers CLIENT-to-SERVER async calls and graphical contracts.
- `docs/decisions/0071-client-resource-lifecycle.md` and
  `docs/decisions/0074-client-resource-executor-seam.md` accept resource
  identity, lifecycle, and completion validation, not source syntax or
  Inspector projection.
- `docs/decisions/0075-std-json-value.md` accepts the append-only V5 JSON value
  snapshot.

The canonical research plan confirms this boundary in
`../spec/docs/49-gap-research-and-contract-plan.md:14-19,39-43,77-93`.

## Post-audit status

The accepted implementation audit found no missing executable slice in the
current locked scope. The canonical status locks the function, invocation,
identity, USER state, Inspector, and reflective-gateway directions
(`../spec/docs/02-status-decisions.md:5-29`), but the executable details for
non-TTY runtimes, resource inspection, and gateways remain proposal-level in
the canonical gap plan (`../spec/docs/49-gap-research-and-contract-plan.md:14-19,39-43`).

The accepted closed slices have focused proof in the repository, including:

- offline source checking and filesystem/network no-write checks in
  `crates/orna-server/tests/source_check.rs`;
- CLIENT capability and resource lifecycle rejection tests in
  `crates/orna-client/src/lib.rs`;
- installed standard-library, capability, and JSON presenter proofs in
  `crates/orna-server/tests/standard_database.rs`;
- framed LSP navigation proof in `crates/orna-lsp/tests/lsp_e2e.rs`.

These tests do not remove the two external evidence blockers: the fresh
network-disabled Debian 12 host proof remains unavailable, and the
same-major embedded-engine transition remains deferred until a real
predecessor edge exists. No proposal-level implementation should start
without the acceptance gate above.

## Non-negotiable contract gate

No implementation phase may start for a proposal-level surface until a new
accepted work ADR and the corresponding canonical status update define all of
these points:

- stable names, identities, and allocation rules;
- exact input, output, and error forms;
- ownership and lifetime rules;
- security, principal, and redaction rules;
- version and compatibility rules;
- failure, cancellation, backpressure, and shutdown behaviour;
- focused tests and one integration or live proof;
- source files, generated files, migrations, and artefacts that must change.

A draft ABI header, narrative example, or traceability row is research evidence,
not an implementation authority.

## Dependency graph

```text
Accepted runtime ABI and CLIENT-to-SERVER contracts
                    |
                    +--> resource transport and runtime events
                    |                 |
                    |                 +--> Inspector resource/UI projections
                    |                                      |
                    |                                      +--> ordinary CLIENT Inspector
                    |                                                           |
                    |                                                           +--> Studio
                    |
                    +--> Exposure, Service, and protocol contracts
                                      |
                                      +--> JSON-RPC gateway
                                      |
                                      +--> MCP gateway

Headless runtime conformance is the first runtime proof.
A production second runtime follows only after the ABI passes conformance.
```

## Phase 0: Contract research and acceptance

These work streams can run in parallel. They must produce evidence and a
reviewable contract draft. They must not edit implementation code.

### 0.1 Runtime ABI and headless conformance contract

Define the first executable runtime boundary from the proposal in
`../spec/api/runtime-abi.md`, `../spec/api/ui-runtime.md`,
`../spec/docs/15-runtime-architecture.md`, and
`../spec/spec/orna_runtime_abi_v1.h`.

The contract must settle:

- canonical value representation and who allocates and releases each buffer;
- descriptor and offer validation, feature negotiation, and version matching;
- surface and node ownership, borrowed input lifetime, and callback lifetime;
- thread affinity, re-entrancy, event-loop ownership, and serialisation;
- atomic UI batch rules, semantic revision handling, and stale-batch rejection;
- typed event payloads, model request completion, and cancellation;
- shutdown ordering, outstanding request handling, and failure reporting.

**Deliverables:** accepted ABI ADR, updated ABI header, conformance fixtures,
and a test-only headless runtime harness. Do not add Qt, GTK, browser, or other
native runtime code in this task.

**Acceptance:** the harness proves descriptor rejection, ownership rules,
atomic batches, stale revisions, typed events, cancellation, and clean
shutdown. The C header passes its syntax check and the Rust client can load and
reject an incompatible fixture deterministically.

**Likely files:** `spec/api/runtime-abi.md`,
`spec/spec/orna_runtime_abi_v1.h`, one new work ADR, and one runtime/client
conformance module per commit.

### 0.2 CLIENT-to-SERVER and asynchronous resource contract

The current resource seam is executor-independent. Define the missing source,
transport, and server execution contract before connecting it to a CLIENT
function.

The contract must settle:

- `RESOURCE`, `AWAIT`, action, stream, and assignment syntax;
- typed request and completion frames, including stream and backpressure rules;
- cancellation, timeout, retry, and shutdown ownership;
- authenticated principal derivation and capability checks;
- function-instance identity, state-session context, and invalidation epochs;
- server execution and audit events for resource requests.

**Deliverables:** accepted language and transport ADRs, parser/compiler
acceptance tests, versioned artefact layout, and one installed host proof.

**Acceptance:** a parameterised CLIENT function can issue one authenticated
request, receive a checked typed result, reject a stale completion, cancel the
request, and leave complete audit evidence. A client cannot provide a
principal, `run_as`, or unchecked result type.

**Likely files:** `spec/spec/orna.ebnf`, `spec/docs/21-resources-actions-streams.md`,
`crates/orna-syntax`, `crates/orna-compiler`, `crates/orna-artifact`,
`crates/orna-client`, `crates/orna-protocol`, `crates/orna-postgres`, and
`crates/orna-server`.

### 0.3 Exposure, Service, and gateway contract

Define the explicit reflective gateway model from
`../spec/docs/19-reflective-gateways.md`,
`../spec/api/protocol-gateways.md`, and
`../spec/docs/27-wire-protocol.md`.

The contract must settle:

- `Endpoint`, `Exposure`, version, `AuthPolicy`, and `ResultPolicy` identities;
- `Service` ownership, launch, lifecycle, and principal/delegation rules;
- external authentication and the rule that the request cannot select a
  principal;
- schema generation and lossless JSON-to-canonical-value conversion;
- version pinning, disabled exposure behaviour, error mapping, streaming, and
  cancellation;
- the exact construction of a canonical sealed `sys.invoke` request.

**Deliverables:** accepted value/API and wire ADRs, standard source and
catalogue identities, bounded protocol frames, conversion tests, and a live
proof for one disabled and one enabled exposure.

**Acceptance:** an external request authenticates outside its body, resolves one
explicit versioned exposure, converts only accepted values, invokes sealed
`sys.invoke`, redacts failures, and cannot inject a principal or bypass grants.
No implicit JSON-RPC or MCP publication is allowed.

**Likely files:** `spec/docs/19-reflective-gateways.md`,
`spec/api/protocol-gateways.md`, `spec/docs/27-wire-protocol.md`, one or more
standard source units, `crates/orna-protocol`, `crates/orna-core`,
`crates/orna-client`, `crates/orna-server`, and `crates/orna-postgres`.

### 0.4 Inspector contract

After 0.1 and 0.2 are accepted, define the ordinary CLIENT Inspector described
by `../spec/docs/30-inspector.md`, `../spec/docs/31-self-inspection.md`, and
`../spec/api/inspect.md`.

The contract must settle:

- the exact `devtools.inspector` CLIENT signature and return value;
- snapshot epoch, freeze, resume, and observer-context semantics;
- recursion and observer suppression for self-inspection;
- immutable projections for CLIENT instances, resources, UI nodes, runtime
  bindings, invocation, presentation, and security plans;
- privilege and redaction rules for arguments, values, source, and audit data;
- the relationship between server snapshot epochs and client runtime epochs.

**Deliverables:** accepted Inspector ADR, checked CLIENT function, versioned
snapshot/projection schema, and installed self-inspection proof.

**Acceptance:** an Inspector root can inspect another invocation without
executing it, can inspect itself without an observer loop, can freeze and
resume an epoch, and cannot read a projection outside its privilege ladder.
Resource and UI rows must contain immutable identity and revision evidence.

**Likely files:** `spec/docs/30-inspector.md`, `spec/docs/31-self-inspection.md`,
`spec/api/inspect.md`, `crates/orna-core/src/inspect.rs`,
`crates/orna-client`, `crates/orna-compiler`, `crates/orna-artifact`,
`crates/orna-server/src/inspect.rs`, and focused live tests.

## Phase 1: Implement the accepted runtime and invocation foundations

Phase 1 starts only after Phase 0 contracts are accepted.

1. Add stable ABI and transport identifiers, with golden encode/decode tests.
2. Add the headless runtime conformance harness and deterministic fixture.
3. Add the CLIENT-to-SERVER request path through the existing authenticated
   invocation boundary.
4. Add resource completion, stream, cancellation, and audit handling.
5. Extend Inspector snapshots with resource and runtime identity rows.

Each step must leave the tree buildable. Keep each commit to one to three
files. Run focused tests after every step, then run `cargo test --workspace
--all-targets` and `just kernel-test` at the phase checkpoint.

## Phase 2: Implement the ordinary CLIENT Inspector

1. Add the parser and checked model for the exact Inspector function surface.
2. Emit a versioned Inspector client plan with stable projection identities.
3. Evaluate snapshot reads through the client runtime without executing the
   observed target.
4. Add UI node and runtime binding projections only through the accepted UI
   runtime contract.
5. Add recursion suppression, privilege checks, redaction, epoch freeze/resume,
   and an installed self-inspection proof.

The proof must cover the Inspector observing a normal function, observing
itself, a denied projection, a stale epoch, and a resource/UI projection with
redacted values.

## Phase 3: Implement Studio and the first production UI runtime

Studio is an ordinary CLIENT application, not a new core language construct.
Implement vertical slices in this order:

1. catalogue tree and function/type search;
2. SQL/source editor with offline diagnostics;
3. result grid backed by typed presenters;
4. source apply, semantic diff, and revision browser;
5. security/DBA page through sealed administration functions;
6. runtime and presenter explorer through Inspector projections;
7. source reload and hot-revision flow with explicit state/session identity.

Select the first production non-TTY runtime only after the conformance harness
passes. The selected runtime must implement the accepted ABI, offer one stable
UI sink, report typed events, and pass the same lifecycle and shutdown suite.

Each Studio slice requires a focused CLIENT proof and one installed end-to-end
proof. Do not add a second toolkit until the first runtime is stable.

## Phase 4: Implement the reflective gateways

1. Register the accepted `std.protocol` and `std.service` value contracts.
2. Add explicit exposure listing, version resolution, schema generation, and
   disabled-exposure rejection.
3. Add one JSON-RPC adapter that authenticates externally and calls sealed
   `sys.invoke`.
4. Add one MCP adapter using the same canonical request boundary and separate
   protocol framing/error tests.
5. Add gateway configuration and inspection through the Studio/Inspector path.

The JSON-RPC and MCP adapters must share conversion and authorisation code but
must keep protocol framing and error mapping separate. Neither adapter may
accept a principal, `run_as`, or arbitrary SQL from an external request.

## Phase 5: Add a second runtime and complete M10-M12 proofs

After the first runtime and Studio are proven:

- implement the second runtime family against the same accepted ABI;
- run the full runtime conformance suite for both runtimes;
- prove automatic selection and explicit override without changing the public
  invocation contract;
- prove Studio source/apply/revision, security, Inspector, presenter, and
  gateway workflows end to end;
- update the traceability matrix only from direct test or artefact evidence.

## Commit and verification policy

For every implementation increment:

- use one to three files per commit;
- use a Conventional Commit subject;
- keep implementation, tests, migrations, and generated identities in coherent
  commits;
- run the focused test before committing;
- run `cargo test --workspace --all-targets` and `just kernel-test` at each
  phase checkpoint;
- run the ABI C syntax check and runtime conformance suite when ABI files change;
- run the clean Debian 12 amd64, network-disabled packaging proof required by
  work ADR 0019 on the required host, not on Fedora or a substitute container;
- do not reformat unrelated files or fabricate a same-major PostgreSQL
  predecessor edge.

## Current blockers

These are specification blockers, not missing implementation effort:

- `spec/api/runtime-abi.md:1-44` and `spec/api/ui-runtime.md:1-47` are
  `CURRENT PROPOSAL` and leave ownership, lifetime, threading, and value
  representation unresolved.
- `spec/docs/49-gap-research-and-contract-plan.md:14-19,39-43` explicitly
  says not to implement graphical runtimes, resource inspection, or reflective
  gateways from the proposal alone.
- `spec/api/inspect.md:1-3` and the self-inspection safety model remain
  `CURRENT PROPOSAL`.
- `spec/api/protocol-gateways.md:1-3` and the wire protocol remain
  `CURRENT PROPOSAL`; Exposure and Service details are not executable.
- Work ADR 0019 requires a clean Debian 12 amd64, network-disabled host proof,
  which is not available on the current Fedora host.
- The checked-in `debian-clean-machine.sh` scenario currently runs its proof
  inside Docker (`crates/orna-system-tests/scenarios/debian-clean-machine.sh:22-27`).
  That is useful isolation evidence but does not satisfy the accepted host
  proof. Obtain a fresh Debian 12 amd64 host or VM runner with networking
  disabled, run the same package and lifecycle matrix without Docker, archive
  machine/package/manifest/process/trace evidence, and update the CI proof path
  before restoring the checked item in `TODO.md`.
- The same-major PostgreSQL upgrade remains intentionally deferred until a
  real successor release declares a predecessor edge.

Until these blockers change status, the correct action is research and plan
maintenance, not implementation. The existing accepted slices remain the
working baseline and must continue to pass their focused and installed-host
gates.
