# OrnaDB remaining implementation plan

## Status and scope

This plan covers the remaining roadmap surfaces after the accepted `orna.std/6`
action snapshot, headless runtime conformance boundary, CLIENT resource and
transport slices, and the headless ordinary CLIENT Inspector v1 and render
contracts.

It tracks implementation hardening, external release evidence, and the
contract work that must precede production graphical runtimes, populated
Inspector projections, Studio, and reflective gateways. The canonical
specification still marks those broader surfaces as `CURRENT PROPOSAL` or
`OPEN`; this plan must not turn them into implementation contracts.

- The external `../TODO.md` checklist records the completed CLIENT, TTY,
  Inspector-core, source, security, presenter, state, resource, action,
  identity, transport, JSON, runtime-conformance, LSP, and verified
  editor/tooling slices, plus the editor-runtime (Neovim/Vim), host-proof, and
  gateway blockers. It is intentionally outside `work/` and outside Git.

- Work ADRs 0068-0079 define the closed CLIENT expression, state, JSON,
  resource, transport, action, and test-only runtime contracts. The accepted
  resource chain is explicit: [0071](docs/decisions/0071-client-resource-lifecycle.md)
  owns lifecycle identity, [0077](docs/decisions/0077-client-server-resource-language.md)
  owns source constructors and `AWAIT`, [0078](docs/decisions/0078-client-server-resource-transport.md)
  owns transport and scheduling, and [0079](docs/decisions/0079-client-action-values.md)
  owns executable `std.action.call` values. Work ADRs [0080](docs/decisions/0080-client-inspector.md)
  and [0081](docs/decisions/0081-standard-inspector-render-contract.md) define
  the headless ordinary CLIENT Inspector v1 and the generic
  `std.inspect.render@1` contract; 0081 supersedes only 0080's product-specific
  helper naming.
- `docs/decisions/0062-std-ui-value-type.md` accepts the transient `std.ui.UI`
  value and TTY runtime offer only. A production graphical runtime remains
  outside the accepted scope.
- `docs/decisions/0064-sys-inspect-core.md`, work ADR 0080, and work ADR 0081
  define the current Inspector boundary. The sealed resource and UI carrier
  identities are accepted, but populated resource/UI rows and richer
  projection semantics remain deferred.
- `docs/decisions/0075-std-json-value.md` and
  `docs/decisions/0079-client-action-values.md` define the append-only V5 JSON
  and V6 action snapshots. Work ADR 0077 owns the accepted CLIENT-to-SERVER
  language surface, with 0078 as its transport/scheduling successor and 0079
  as its executable action successor; these ADRs do not accept the proposal-
  level model, gateway, or runtime surfaces.

The canonical research plan remains useful as historical evidence, but this
plan is the current implementation projection. Accepted ADRs and
`../spec/docs/02-status-decisions.md` remain authoritative.

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
   install, and invoke path as user programs. Resource model/consumer
   semantics beyond the accepted scalar and stream forms, graphical runtimes,
   populated Inspector resource/UI projections, and reflective gateways stay
   out of these fixtures until their contracts become executable.

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

The 2026-08-22 locked-scope audit found no missing accepted executable slice.
It did find and close evidence and tooling defects without broadening the
contract:

- retained V5 JSON and V6 action source, catalogue, digest, and codec parity
  now have focused goldens in `crates/orna-standard/src/lib.rs`;
- canonical scalar spellings now have parser, LSP, and tree-sitter coverage;
- the Inspector ORV5 carrier rejects duplicate and descending record-field
  identities;
- the lifecycle verifier accepts only the exact normal or SIGQUIT-escalated
  stop sequence and derives the escalation report flag from that sequence;
- the Zed integration now registers the real grammar and the existing
  `orna-lsp` stdio binary;
- direct resource revision pinning was audited against the accepted locks and
  needs no source change.
- bare `has_privilege` checks now fail closed for inactive principals and reject
  object-scoped INSPECT requests; the installed security-admin proof covers both
  enforcement boundaries.
- applied migration SQL remains append-only; commit `a4d6ff0` restored the
  exact version-23, version-29, and version-30 bytes after the live bootstrap
  gate detected a checksum mismatch. The focused bootstrap proof and the
  historical local/default command `cargo test --workspace --all-targets`
  passed; `just kernel-test` is Docker Compose/ignored PostgreSQL evidence,
  not current clean Debian-host proof.
- the installed source-apply path now records one fixed-principal protected
  `SourceApply` event, and audit recovery checks both the principal and the
  historical source/catalogue pair;
- retained revision-pair listing now validates all decoded ancestry in memory,
  including parent parity, cycles, identity uniqueness, and the single active
  marker, without per-entry database round trips;
- commit `6e61996` now enforces retained V1 source identity before checking the
  source: the canonical source-unit ID, `std/types.orna` logical path, and
  ordinal are required. Local evidence is
  `crates/orna-compiler/src/resolver.rs::tests::rejects_v1_source_unit_identity_mutations`.
- commit `c58869d` now orders checked resource and action arguments by their
  canonical `CheckedParameterId`, independent of source named-argument order.
  Local evidence is
  `crates/orna-compiler/src/resolver.rs::tests::sorts_resource_and_action_arguments_by_checked_parameter_id`.
- commit `ba20fda` now proves private headless runtime teardown retires owned
  node, action, and operation handles, cancels owned requests once, and rejects
  stale work. Local evidence is
  `crates/orna-client/src/lib.rs::runtime_conformance::destroying_a_surface_retires_all_owned_handles_and_suppresses_stale_work`
  (the focused runtime-conformance run passed 39 tests); this is not production
  runtime-ABI evidence.
- commit `1aeef47` now proves the accepted SERVER dogfood fixture checks and
  prepares offline, including its four functions and candidate revision pair.
  Local evidence is
  `crates/orna-server/tests/standard_database.rs::checks_and_prepares_server_function_dogfood_fixture_offline`;
  PostgreSQL installation and invocation remain Compose-gated.
- commit `8af410c` adds a focused source-apply recovery-mismatch mapping
  regression; local source-apply fail-closed coverage now includes the exact
  post-apply candidate-hash reproduction guard.
- commit `64d9603` covers both retained legacy Inspector public error-code
  aliases; the accepted Inspector boundary still has no populated projection
  or graphical-runtime claim.

The accepted slices have focused proof in the repository. The resource,
transport, action, and Inspector slices have local parser/compiler/codec/
carrier checks plus installed proof paths; the installed paths that exercise
PostgreSQL are marked `#[ignore]` because they require the Compose PostgreSQL
development service. No local Compose result is claimed. The source-apply
audit, rollback, tamper, and retained-listing integration tests are likewise
present but Compose-gated. Local evidence covers compilation, migration
registry checks, codec checks, and focused in-memory validators only.
The four commits above add local compiler, private-fixture, and offline
SERVER-dogfood evidence only; they do not upgrade any Compose or host-gated
proof, or broaden the accepted boundary.
The fresh network-disabled Debian 12 host proof and the same-major PostgreSQL
predecessor transition remain separate blockers. No proposal-level
implementation should start without the contract gate below.

## 2026-08-22 next contract checkpoint

The previously recorded 2026-08-19 checkpoint is superseded by work ADRs
0076-0081. The current boundary is:

- the test-only headless runtime conformance fixture is accepted and
  implemented; the production runtime ABI remains a proposal;
- CLIENT resource language, transport, and scheduling are accepted and
  implemented under [0077](docs/decisions/0077-client-server-resource-language.md)
  and [0078](docs/decisions/0078-client-server-resource-transport.md). Focused
  parser/compiler/codec checks are local; installed resource/transport proofs
  in `crates/orna-server/tests/standard_database.rs` are Compose-gated
  `#[ignore]` tests, so no local Compose result is claimed.
- Executable actions are limited to `std.action.call` under
  [0079](docs/decisions/0079-client-action-values.md); focused plan/trigger
  checks are present, while the installed SERVER-action proof is also
  Compose-gated. `std.action.sequence` and `std.action.parallel` remain
  reserved until a later scheduler contract.
- The ordinary CLIENT Inspector v1 and generic render contract are accepted
  and implemented under [0080](docs/decisions/0080-client-inspector.md) and
  [0081](docs/decisions/0081-standard-inspector-render-contract.md). Focused
  carrier/epoch/lineage checks are local; installed evaluator, recursion, and
  cross-principal proofs are Compose-gated. Populated resource/UI projections
  remain outside this contract.
- reflective gateways remain on the sealed `sys.invoke` boundary until
  Endpoint, Exposure, Service, authentication, conversion, redaction, and
  protocol lifecycle contracts are accepted.

The next implementation plan is therefore conditional, not an instruction to
start code:

1. Accept one new contract and update the canonical status, work ADR index,
   traceability, and exact source/generated artefact list.
2. Add stable identities, exact errors, ownership, security, compatibility,
   cancellation, and shutdown rules before implementation.
3. Add one focused unit slice and one integration or live proof.
4. Implement the smallest vertical path, then run the package gate and the
   installed proof before extending the surface.

The candidate order is:

1. production runtime ABI and one non-TTY runtime, after the canonical ABI
   resolves ownership, re-entry, thread, event, and shutdown rules;
2. populated Inspector resource/UI projections, after the contract resolves
   epoch ownership, lifecycle capture, privilege, and redaction;
3. reflective gateway contracts and adapters, after the external
   authentication, exposure versioning, conversion, and protocol rules are
   executable.

Until one candidate is accepted, close the Debian host evidence blocker or
maintain the current baseline. Do not implement a graphical runtime, populate
Inspector projections, or add a JSON-RPC/MCP gateway from proposal text alone.

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
Accepted headless runtime conformance
 |
 +--> [contract gate] production runtime ABI and non-TTY runtime
 |                                      |
 |                                      +--> Studio
 |
 +--> [contract gate] populated Inspector resource/UI projections
                                        |
                                        +--> Studio Inspector explorer
 |
 +--> [contract gate] Exposure, Service, and protocol contracts
                                        |
                                        +--> JSON-RPC gateway
                                        |
                                        +--> MCP gateway
```

The accepted headless runtime and ordinary CLIENT Inspector are baselines.
They do not imply acceptance of the production runtime, populated projections,
Studio, or gateways.

## Phase 0: Contract research and acceptance

Phase 0 is a contract gate. Sections 0.1-0.3 research and accept new
contracts. Section 0.4 hardens the already accepted Inspector contracts. No
proposal-level implementation belongs in this phase.

### 0.1 Production runtime ABI and non-TTY runtime contract

The test-only headless runtime conformance fixture is accepted and implemented.
Define the next production boundary from the proposal in
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

**Deliverables:** accepted production ABI ADR, updated ABI header, one selected
non-TTY runtime contract, conformance fixtures, and a runtime proof. Do not
select Qt, GTK, browser, or other native runtime code before the contract is
accepted.

**Acceptance:** the selected runtime loads through the accepted ABI, rejects
incompatible descriptors, obeys ownership and thread rules, applies atomic
batches, rejects stale revisions, reports typed events, handles cancellation,
and shuts down cleanly. The proof must cover the same lifecycle cases as the
headless fixture.

**Likely files:** `spec/api/runtime-abi.md`,
`spec/spec/orna_runtime_abi_v1.h`, one new work ADR, and the selected runtime
and client modules.

### 0.2 Populated Inspector resource and UI projection contract

The accepted Inspector v1 exposes immutable headless carriers, including the
resource and UI carrier identities. Current installed fixtures keep the
resource/UI rows empty. Define populated resource and UI projection semantics
before adding data to the Inspector or Studio.

The contract must settle:

- resource, stream, UI surface, node, and runtime identity schemas;
- snapshot epoch ownership, capture timing, and freeze/resume behaviour;
- projection revision, invalidation, and stale-read handling;
- privilege, principal, source, argument, value, and audit redaction rules;
- bounded carrier sizes, ordering, truncation, and error forms;
- observer suppression and recursion rules for resource and UI inspection;
- the relationship between server epochs and client runtime epochs.

**Deliverables:** accepted populated-projection ADR, versioned carrier schema,
compiler and artefact identities, focused carrier tests, and one installed
Inspector proof with redacted resource/UI data. The proof is an environment-
dependent Compose integration proof and must not be reported as locally run
until that service is available.

**Acceptance:** an ordinary CLIENT Inspector can request one permitted
resource or UI projection, receive an immutable checked carrier, reject stale
or over-limit data, suppress observer recursion, and redact values outside the
privilege ladder. The proof must not require a graphical UI sink.

**Likely files:** `spec/docs/30-inspector.md`,
`spec/docs/31-self-inspection.md`, `spec/api/inspect.md`,
`crates/orna-core/src/inspect.rs`, `crates/orna-artifact`,
`crates/orna-client`, `crates/orna-server`, and focused live tests.

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
versioned snapshot/projection schema, and the Compose-gated installed
self-inspection proof path.

**Acceptance:** the headless Inspector root continues to inspect another
invocation without executing it, inspect itself without an observer loop,
freeze and resume an epoch, and reject projections outside its privilege
ladder. These checks do not accept a graphical runtime/UI sink, populated
resource/UI projections beyond the accepted headless scope, or reflective
gateways.

**Likely files:** `crates/orna-core/src/inspect.rs`, `crates/orna-client`,
`crates/orna-compiler`, `crates/orna-artifact`,
`crates/orna-server/src/inspect.rs`, and focused live tests.


## Phase 1: Implement the next accepted contract

Phase 1 starts only after one Phase 0 contract is accepted and its exact
source, generated artefact, and proof list is recorded.

1. Add stable identities, exact errors, ownership, security, compatibility,
   cancellation, and shutdown rules.
2. Add one focused unit slice and one integration or live proof.
3. Implement the smallest vertical path through the existing accepted
   boundary.
4. Keep the tree buildable and preserve all accepted baselines.

Keep each commit to one to three files. Run focused tests after every step,
then run the workspace and installed gates at the phase checkpoint.

## Phase 2: Maintain the headless ordinary CLIENT Inspector baseline

ADRs 0080 and 0081 accept and current HEAD delivers the headless ordinary
CLIENT Inspector v1 and generic standard render contract
`std.inspect.render@1`. Focused local checks are present; the installed
self-inspection/evaluator proofs remain Compose-gated and are not claimed as
locally run.

The baseline work is complete. Further work in this phase is conditional on an
accepted projection contract:

1. preserve the checked Inspector signature and stable projection identities;
2. preserve snapshot reads without executing the observed target;
3. preserve recursion suppression, privilege checks, redaction, epoch
   freeze/resume, and the Compose-gated installed self-inspection proof path;
4. add populated resource/UI rows only after the Phase 0.2 contract is accepted.

The current local proof covers the accepted headless scope. The installed proof
paths remain Compose-gated and must not be described as locally run, or as
proof of a graphical runtime, populated resource/UI projections, or reflective
gateways.

## Phase 3: Implement Studio and the first production UI runtime

This phase is blocked until the production runtime and populated Inspector
projection contracts are accepted. Studio remains an ordinary CLIENT
application, not a new core language construct.

After those contracts are accepted, implement vertical slices in this order:

1. catalogue tree and function/type search;
2. SQL/source editor with offline diagnostics;
3. result grid backed by typed presenters;
4. source apply, semantic diff, and revision browser;
5. security/DBA page through sealed administration functions;
6. runtime and presenter explorer through Inspector projections;
7. source reload and hot-revision flow with explicit state/session identity.

Each Studio slice requires a focused CLIENT proof and one installed end-to-end
proof. Do not add a second toolkit until the first runtime is stable.


## Phase 4: Implement the reflective gateways

This phase is blocked until the Exposure, Service, authentication, conversion,
redaction, and wire lifecycle contracts are accepted.

After acceptance:

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

This phase is blocked until Phase 3 has a proven production runtime and Studio
path, and Phase 4 has accepted gateway contracts.

After those prerequisites:

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

These are contract and external-evidence blockers, not missing implementation
effort:

- `spec/api/runtime-abi.md:1-44` and `spec/api/ui-runtime.md:1-47` remain
  `CURRENT PROPOSAL` and leave production ownership, lifetime, threading, and
  value representation unresolved.
- `spec/docs/30-inspector.md` and `spec/docs/31-self-inspection.md` lock the
  headless carrier and recursion boundaries, but `spec/api/inspect.md` remains
  `CURRENT PROPOSAL` for populated resource/UI projection semantics.
- `spec/api/protocol-gateways.md:1-3` and the wire protocol remain
  `CURRENT PROPOSAL`; Endpoint, Exposure, Service, authentication, conversion,
  redaction, and lifecycle details are not executable.
- Work ADR 0019 requires a clean Debian 12 amd64, network-disabled host proof,
  which is not available on the current Fedora host.
- Installed PostgreSQL checks remain Compose-gated `#[ignore]` evidence; no
  local Compose result is claimed for source apply, Inspector, resource,
  action, security-admin, or invocation proofs.
- Offline editor tooling is accepted, but the Neovim/Vim editor-runtime checks
  remain blocked when those binaries are unavailable; no editor-runtime proof
  is claimed.
- Studio remains pending behind the production runtime ABI and populated
  Inspector projection contracts; no Studio source/apply/revision path is
  accepted yet.
- The checked-in `debian-clean-machine.sh` scenario currently runs its proof
  inside Docker (`crates/orna-system-tests/scenarios/debian-clean-machine.sh:22-27`).
  That is useful isolation evidence but does not satisfy the accepted host
  proof. Obtain a fresh Debian 12 amd64 host or VM runner with networking
  disabled, run the same package and lifecycle matrix without Docker, archive
  machine/package/manifest/process/trace evidence, and update the CI proof path
  before restoring the checked item in `../TODO.md`.
- The same-major PostgreSQL upgrade remains intentionally deferred until a
  real successor release declares a predecessor edge.

Until these blockers change status, maintain the accepted implementation
baseline, improve its evidence, and prepare the next contract. Do not
implement a graphical runtime, populated Inspector projections, Studio, or
JSON-RPC/MCP gateways from proposal text alone.
