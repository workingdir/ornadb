# OrnaDB remaining implementation plan

## Status and scope

This plan covers the remaining roadmap surfaces after the accepted `orna.std/5`
JSON value slice and the CLIENT resource and USER state slices.

The plan tracks implementation hardening and integration after the accepted
headless ordinary CLIENT Inspector v1 in work ADR 0080 and the generic standard
render contract `std.inspect.render@1` accepted by work ADR 0081. The current
specification still marks the graphical runtime ABI, UI event boundary,
populated resource/UI projections beyond that accepted scope, and reflective
gateway details as `CURRENT PROPOSAL` or `OPEN`; the repository must not treat
those drafts as implementation contracts.

Current accepted implementation evidence:

- `TODO.md:54-89` records the completed CLIENT, TTY, Inspector-core, source,
  security, presenter, state, resource, identity, transport, and JSON slices;
  `TODO.md:90-94` records the delivered headless ordinary CLIENT Inspector v1
  and the accepted generic standard render contract `std.inspect.render@1`.
- `docs/decisions/0062-std-ui-value-type.md` accepts the transient `std.ui.UI`
  value only. It defers the graphical runtime ABI and UI sink.
- `docs/decisions/0063-automatic-runtime-selection.md` accepts the TTY offer and
  deterministic selection only. Non-TTY runtimes remain later work.
- `docs/decisions/0064-sys-inspect-core.md` accepts server-side snapshots and
  the `orna inspect` CLI. Work ADRs 0080 and 0081 accept the headless ordinary
  CLIENT Inspector v1 and generic standard render contract `std.inspect.render@1`;
  populated resource/UI projections beyond that scope remain deferred.
- `docs/decisions/0068-client-expression-bodies.md` accepts closed CLIENT
  expressions but defers CLIENT-to-SERVER async calls and graphical contracts.
- `docs/decisions/0071-client-resource-lifecycle.md` and
  `docs/decisions/0074-client-resource-executor-seam.md` accept resource
  identity, lifecycle, and completion validation. Work ADRs 0080 and 0081 accept
  the headless ordinary CLIENT Inspector v1 and generic standard render contract;
  broader populated projections remain deferred.
- `docs/decisions/0075-std-json-value.md` accepts the append-only V5 JSON value
  snapshot.

The canonical research plan confirms this boundary in
`../spec/docs/49-gap-research-and-contract-plan.md:14-19,39-43,77-93`.

## Language and standard-library dogfooding

The answer to the current concern is yes: future work must make Orna
programs more useful as source, not only as examples around Rust-backed kernel
contracts. The current `stdlib/std/*.orna` files are intentionally small
because the accepted slices still expose sealed host boundaries. That is a
valid boundary for implementation, but it is not a sufficient language
dogfooding story.

This work has three separate goals:

1. **Grammar integration.** Keep `spec/spec/orna.ebnf` as the canonical
   proposal source, then connect accepted productions to parser fixtures,
   syntax-tree tests, diagnostics, editor grammars, and runnable source
   examples. Do not generate or freeze the parser from the current proposal
   until the grammar status changes.
2. **Standard-library source parity.** For every accepted standard function or
   value contract, prefer a real `.orna` declaration and body where the
   language can express it. Keep Rust code only at explicit kernel, codec,
   security, storage, or host-runtime boundaries. Each remaining sealed
   boundary must have a documented reason and a focused proof.
3. **Runnable dogfood applications.** Add small source programs that use the
   accepted language and standard library through the same parse, check,
   install, and invoke path as user programs. Proposal-only resources,
   graphical runtimes, populated Inspector resource/UI projections, and reflective
   gateways stay out of these fixtures until their contracts become executable.

- use the existing accepted CLIENT expression and state syntax as the first
  source base, then extend it only when the corresponding contract is accepted;
- add a grammar conformance gate for the accepted subset;
- add one complete accepted server-function dogfood program, then identify the
  next presenter or launcher path that can move into `.orna` source;
- make each source program run through an installed proof;
- then repeat the same pattern for accepted resource and later Inspector or
  gateway contracts as each contract becomes executable.

This is a product requirement, not optional documentation polish. A fully
featured language needs source users can read, modify, check, and run.

## Post-audit status

The accepted implementation audit found no missing executable slice in the
current locked scope. The canonical status locks the function, invocation,
identity, USER state, Inspector, and reflective-gateway directions
(`../spec/docs/02-status-decisions.md:5-29`), but the executable details for
non-TTY runtimes, populated Inspector resource/UI projections, and gateways
remain proposal-level in
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

## 2026-08-19 contract research checkpoint

A read-only review of the three deferred contract surfaces and an independent
locked-scope audit found no missing accepted executable slice. The current
runtime header passes its C syntax check, but its semantic contract remains
open.

Evidence and boundaries:

- Runtime ABI: `../spec/api/runtime-abi.md`,
  `../spec/api/ui-runtime.md`, and
  `../spec/spec/orna_runtime_abi_v1.h` leave value ownership, callback
  lifetime, thread and re-entry rules, atomic batches, typed events,
  cancellation, and shutdown behaviour unresolved. Work ADR 0076 is a
  proposal only.
- CLIENT-to-SERVER resources: `../spec/docs/21-resources-actions-streams.md`
  is a current proposal. ADRs 0071 and 0074 provide only the
  executor-independent identity, generation, completion, and stale-result
  checks. They do not define source syntax, transport frames, scheduling,
  server execution, audit events, or stream backpressure.
- Reflective gateways:
  `../spec/docs/19-reflective-gateways.md`,
  `../spec/api/protocol-gateways.md`, and
  `../spec/docs/27-wire-protocol.md` lock the direction but not the
  Endpoint, Exposure, Service, authentication, version pinning, conversion,
  redaction, or protocol lifecycle contracts. Existing code must remain on
  the sealed `sys.invoke` boundary.
- Locked scope: the accepted CLIENT, USER state, resource lifecycle,
  identity/ORV6, JSON V5, security-admin, presenter, Inspector-core,
  source-diff, LSP, and embedded slices have implementation and focused proof
  evidence. The remaining embedded items are external evidence or a future
  predecessor release, not code gaps.
Follow-up scope checks for language/compiler, storage/transactions,
security/authentication, and tooling/operations found the same result:
accepted contracts have implementation and focused proof evidence. The
legacy unchecked rows in `../spec/IMPLEMENTATION_CHECKLIST.md` do not
override the canonical status or accepted work ADRs. No accepted code gap is
available for the next implementation step; work must either close the
recorded Debian host evidence blocker or wait for an accepted proposal
contract.

The next contract order is:

1. Accept the test-only headless runtime conformance boundary in ADR 0076.
   It is the smallest reversible contract and is a prerequisite for runtime
   events and later Inspector projections.
2. Accept the CLIENT-to-SERVER asynchronous resource contract, including
   syntax, versioned request/completion frames, principal derivation,
   capability checks, cancellation, backpressure, shutdown, and audit events.
3. Harden and integrate the accepted headless ordinary CLIENT Inspector v1
   after the runtime and resource contracts define its epoch and redaction
   inputs.
4. Accept the reflective gateway contract separately, then implement its
   disabled/enabled exposure proof through sealed `sys.invoke`.

No implementation starts for these surfaces until the acceptance gate below
has an accepted contract, exact error and ownership rules, compatibility
rules, cancellation behaviour, focused tests, an integration or live proof,
and an explicit file and artefact list.

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

Work ADRs 0080 and 0081 accept the headless ordinary CLIENT Inspector v1 and
its generic standard render contract `std.inspect.render@1`, described by
`../spec/docs/30-inspector.md`, `../spec/docs/31-self-inspection.md`, and
`../spec/api/inspect.md`. This stream now covers implementation hardening and
integration, not acceptance of a proposal-level Inspector contract.

Implementation hardening and integration must preserve:

- the accepted ordinary CLIENT Inspector signature and return value, with
  rendering identified by `std.inspect.render@1` rather than an application
  function name;
- snapshot epoch, freeze, resume, and observer-context semantics;
- recursion and observer suppression for self-inspection;
- immutable projection-carrier identity and revision evidence;
- privilege and redaction rules for arguments, values, source, and audit data;
- the relationship between server snapshot epochs and client runtime epochs.

**Deliverables:** focused hardening changes, checked CLIENT function,
versioned snapshot/projection schema, and installed self-inspection proof.

**Acceptance:** the headless Inspector root continues to inspect another
invocation without executing it, inspect itself without an observer loop,
freeze and resume an epoch, and reject projections outside its privilege
ladder. These checks do not accept a graphical runtime/UI sink, populated
resource/UI projections beyond the accepted headless scope, or reflective
gateways.

**Likely files:** `crates/orna-core/src/inspect.rs`, `crates/orna-client`,
`crates/orna-compiler`, `crates/orna-artifact`,
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

ADRs 0080 and 0081 accept and current HEAD delivers the headless ordinary
CLIENT Inspector v1 and generic standard render contract `std.inspect.render@1`.
Remaining work is implementation hardening and integration:

1. Harden the checked Inspector function and versioned client plan against the
   accepted signature, generic render contract, and stable projection identities.
2. Verify snapshot reads through the client runtime without executing the
   observed target.
3. Preserve recursion suppression, privilege checks, redaction, epoch
   freeze/resume, and the installed self-inspection proof.
4. Keep graphical runtime/UI sink integration, populated resource/UI
   projections beyond the accepted headless scope, and reflective gateways
   outside this phase until their contracts are accepted.

The proof must cover the Inspector observing a normal function, observing
itself, a denied projection, a stale epoch, and a bounded resource/UI carrier
case with redacted values within the accepted headless scope.

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
