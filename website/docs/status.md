---
title: Status
description: OrnaDB 1.0 CLIENT VM boundary and repository implementation status.
---

# CLIENT VM status

This page separates normative OrnaDB 1.0 behavior from implementation work in
the repository. The frozen OrnaDB 1.0.0 reference is authoritative for language
and runtime guarantees.

## OrnaDB 1.0 boundary

OrnaDB 1.0 defines explicit remote source evaluation: source runs only through
an explicit `eval` or `watch` operation, and the host parses, resolves,
type-checks, and executes it with the same language implementation and
activation semantics as the local REPL. A client-supplied AST, bytecode, or
query plan is not authoritative (**ORNA-EVAL-001** and **ORNA-EVAL-002**,
`source/14-pages.md`, “Explicit remote Orna evaluation”). Watches retain their
read-only effect boundary (**ORNA-EVAL-004**, same section).

The trusted v1 deployment boundary is the invoking OS user and a trusted
network perimeter (**ORNA-TRUST-001** through **ORNA-TRUST-003**,
`source/26-security.md`, “Trust, isolation and extensions”). Build metadata
identifies the source snapshot and compatibility coordinates; that requirement
does not define artifact signatures or provenance (**ORNA-SYS-072**,
`source/15-system.md`, “History, plans and storage”). Native extensions run
with host-process trust and are not sandboxed by Orna (**ORNA-EXT-005**,
`source/26-security.md`, “Extensions”).

The OrnaDB 1.0 reference does not define a CLIENT bytecode VM contract, artifact
admission or loading protocol, CLIENT capability broker, signature or
provenance policy, or CLIENT sandbox guarantee. The Stage 1 implementation
described below is therefore repository implementation evidence; it does not
establish an accepted OrnaDB 1.0 security guarantee. See the
[CLIENT VM trust-boundary decision](../../docs/decisions/0101-client-vm-trust-boundary.md)
for the bounded reference search and deferral rationale.

## Stage 1 repository implementation

The repository contains bounded control-plane seams for:

- structural admission of immutable CLIENT artifact and plan evidence;
- non-zero invocation identities;
- an immutable runtime-offer witness and canonical digest; and
- ephemeral in-memory capability-lease lifecycle state with policy and
  cancellation fences.

These seams do not execute production host effects, call kernel audit, issue
production capability leases, verify signatures or provenance, or provide
operating-system isolation. They must not be described as a production CLIENT
VM, sandbox, or host-effect broker. The production CLIENT VM trust and sandbox
remain **DEFERRED**; Work ADR 0091 remains a **CURRENT PROPOSAL**, not accepted
OrnaDB 1.0 behavior.
