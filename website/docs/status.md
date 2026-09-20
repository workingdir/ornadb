---
title: Status
description: Implemented repository work, accepted bounded slices, locked decisions, deferred proposals, open questions, and future work.
---

# Status

This page separates what exists from what is designed. It summarises the
repository delivery checklist and the canonical v0.2 design status.

:::warning Development status
OrnaDB is under active development. The repository builds and verifies local
development slices, but no public release is available to install today.
:::

## Implemented repository work

These items are implemented, reviewed, and verified locally in the working
repository:

| Item | Status |
|---|---|
| Offline `orna source check <file.orna>` | IMPLEMENTED |
| SERVER query and mutation slices through required unique reference fields | IMPLEMENTED |
| First verified Boolean CLIENT function path | IMPLEMENTED |
| Stable catalogue value-type and binding identities | IMPLEMENTED |
| Versioned standard-library and catalogue hashes without changing version-1 bytes | IMPLEMENTED |
| Source-independent standard type manifest | IMPLEMENTED |
| Orna-owned instance, initialisation, private socket, and foreground supervision | IMPLEMENTED |
| Native `orna server backend-shell` | IMPLEMENTED |
| Stage 1 CLIENT VM structural artifact admission with bounded immutable plan evidence | IMPLEMENTED |
| Stage 1 non-zero invocation identity allocation with in-memory collision and release control | IMPLEMENTED |
| Stage 1 immutable runtime-offer witness and canonical digest | IMPLEMENTED |
| Stage 1 ephemeral in-memory capability-lease control-plane state machine and policy/cancellation fences | IMPLEMENTED |

These are contract and control-plane slices in the repository. They do not yet
form a production CLIENT VM or a usable product. Stage 1 performs no production
host effect, kernel audit call, signature verification, production lease
issuance, or operating-system isolation.

## Accepted bounded slices

The following source and contract slices are accepted, but do not imply a public
release. Environment-gated proofs remain deferred.

- **Qt v1 runtime/provider/package — ACCEPTED (BOUNDED).** The first production non-TTY provider is `orna-runtime-qt` on Linux x86_64: Qt 6 Widgets, ABI v1.0, and caller-pumps. It is a separately installed package with a fixed path and Debian repository authentication; the local `orna` client selects an installed offer. A test-only headless fixture shares the ABI v1 semantic contract.
- **TTY/presenter/output — ACCEPTED (BOUNDED).** `orna-runtime-tty` is the accepted terminal renderer. Typed presenter planning and optional `--output` are accepted; TTY is a runtime, while JSON, CSV, and XML are encoded outputs.
- **Scalar and `STREAM<T>` resources — ACCEPTED (BOUNDED).** Explicit typed resource construction is executable for scalar targets and `STREAM<T>` targets. `TABLE`/`ROWS` resource transport is deferred.
- **`std.json`/UI/action — ACCEPTED (BOUNDED).** `std.json.Value`, transient UI contracts, and bounded `std.action.call` are accepted. Sequence and parallel actions remain deferred.
- **V8 Rows/table presentation — ACCEPTED (BOUNDED).** `std.data.Rows` V8 codecs and retained table/CSV presentation are accepted. General Rows/object-value semantics remain deferred.
- **Bounded populated Inspector slices — ACCEPTED (BOUNDED).** ADR 0086 records the headless Inspector v1 resource kind/status, UI identity, final-presentation, and runtime-offer projections captured at the immutable epoch boundary. It does not accept resource request/value or stream identity, a full UI tree, native runtime handles, models, live refresh, gateways, or Studio semantics.

## Not yet implemented

| Area | Item | Status |
|---|---|---|
| Protocol | Public protocol, authorisation, and exposure slices | DEFERRED |
| Types | Enum, record, and opaque value types beyond standard primitives; general `VALUE` semantics | DEFERRED |
| CLIENT VM | Production sandbox, concrete filesystem/network/secret host capabilities, host-effect broker, and process isolation | DEFERRED |
| Security | Protected audit path and production/integration audit proof for CLIENT host effects | DEFERRED |
| Trust | Signed artifact identity/provenance, keyring, and cryptographic attestation | DEFERRED |
| Gateways | Reflective JSON-RPC/MCP gateway implementation and exposure dispatch | DEFERRED |
| Launch | `std.launch` and launch/application execution | DEFERRED |
| Data | Virtual models and `TABLE`/`ROWS` resource transport | DEFERRED |
| Dogfooding | Full Studio and security/DBA UI | DEFERRED |
| Proof | Environment-gated Compose, installed-runtime, and clean-host proofs | DEFERRED |

## Current proposals

These are concrete designs for future implementation experiments. They are not
released and remain outside the accepted bounded slices:

- **CURRENT PROPOSAL:** the full production CLIENT VM, including its sandbox and
  capability host-effect broker;
- protected audit integration for CLIENT capability decisions and effects;
- signed identity-bound artifact attestation and provenance;
- process isolation for any future untrusted native, JIT, FFI, or plugin surface;
- reflective JSON-RPC/MCP gateways and `std.launch`;
- virtual `TableModel`/`TreeModel` models and `TABLE`/`ROWS` resource transport;
- general `VALUE` and object-value semantics beyond the accepted Rows contract;
- presenter registry/ranking and runtime ABI/toolkit extensions beyond Qt v1;
- module and package distribution beyond the fixed Qt runtime package.

## Locked design decisions

| Area | Decision |
|---|---|
| Product | OrnaDB, “Object-Relational Native Applications”; CLI is `orna` |
| Executables | Every executable definition is a function |
| Domains | `CREATE SERVER FUNCTION` and `CREATE CLIENT FUNCTION`; one domain per function |
| Applications | A running program is a rooted function invocation graph; no `CREATE APPLICATION` |
| UI type | `std.ui.UI`, a standard-library transient value type |
| UI entry | `std.ui.window(title TEXT, content std.ui.UI)` as `std.ui.window@1` |
| JSON value | Immutable transient `std.json.Value` |
| Invocation | Root calls go through inspectable `sys.invoke` |
| Runtime | Local `orna` selects an installed runtime offer; the first production non-TTY provider is bounded `orna-runtime-qt` |
| Identity | `sys.security.session_principal()` and related functions; no `CURRENT_USER` keyword |
| State | Durable `USER` state keyed by authenticated principal |
| Resources | Typed `std.data.Resource<T>` and `std.data.StreamResource<T>` with explicit `AWAIT` |
| Actions | Executable v1 action is `std.action.call`; sequence and parallel remain deferred |
| Inspector | An ordinary CLIENT function using public introspection APIs |
| Security | Principals are first-class catalog data with kernel enforcement |
| Runtimes | Explicitly installed client libraries; server never selects native code |

## Open questions

- CLIENT VM bytecode versus WASM versus custom IR;
- exact production sandbox and host-effect audit boundary;
- signed artifact envelope, keyring, rotation, revocation, and replay policy;
- process isolation requirements for future native or untrusted code;
- physical storage layout and PostgreSQL wire compatibility.

## Sources of truth

The design bundle holds the canonical handbook and ADRs. The [source
repository](https://github.com/workingdir/ornadb) is the authoritative location
for the implementation.

Return to the [OrnaDB overview](/) or inspect the [source repository](https://github.com/workingdir/ornadb).
