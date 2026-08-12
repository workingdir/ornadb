# ADR 0022: CLIENT Evaluation Requires an Authorised Invocation

**Status:** Accepted

## Decision

The local CLIENT evaluator no longer accepts a bare `FunctionId`. Its public
interface requires an `AuthorisedInvocation` produced by the security decision
core from ADR 0020.

Before inspecting catalogue semantics or artefact bytes, the evaluator requires
the decision's pinned `RevisionPair` to equal the supplied active revision. It
then selects only the decision's `FunctionId`. A decision for another revision
or function cannot be reused by changing a separate function argument because
no separate function argument exists.

An authorisation mismatch is a typed `ClientExecutionError` and occurs before
active-catalogue hashing, function lookup, artefact decoding, or evaluation.
The error exposes the authorised target and the supplied active pair, but no
credential or internal PostgreSQL fact.

This makes the evaluator's post-authorisation status part of its Rust type
interface instead of a documentation convention. `AuthorisedInvocation` has no
public constructor; the core security decision is the only source of an allowed
value.

## Kernel gate

The private PostgreSQL kernel provides the server-facing CLIENT evaluation
operation. In one repeatable-read transaction it:

1. requires current migrations;
2. recovers the active application revision;
3. recovers the durable security snapshot against that same revision;
4. revalidates the already-authenticated session in the recovered snapshot;
5. authorises the requested stable `FunctionId` at the active `RevisionPair`;
6. evaluates only an allowed invocation; and
7. commits and closes the PostgreSQL session before returning the typed result.

A denial is returned as a typed kernel error and the CLIENT evaluator is not
called. The operation accepts an `AuthenticatedSession`, not a caller-supplied
principal identity. A later transport adapter owns session establishment and
must never decode a principal or active-role list from invocation payload bytes.

## Required proof

Tests must prove:

* the evaluator accepts allow evidence for the exact active function and pair;
* evidence for another pair is rejected before active-revision validation;
* the public evaluator has no bare-function overload;
* the kernel gate evaluates the granted Boolean CLIENT function;
* missing, revoked, disabled, stale-session, and unknown-function requests are
  denied without invoking the evaluator;
* the decision and returned execution contexts bind the same function and
  revision pair; and
* every success and failure path closes its PostgreSQL session.

Normal format, strict Clippy, rustdoc, diff, similarity, and live PostgreSQL
gates remain required.

## Deferred surface

This record does not establish credentials or authenticate a socket. It does
not accept a public value codec, raw call frame, protocol version, cancellation,
streaming, `sys.invoke`, `orna invoke`, `SECURITY DEFINER`, capability checks,
audit storage, or CLIENT artefact transport.

Each requires a later accepted decision and fail-closed implementation.

## Precedence

This closes ADR 0015's deferred post-authorisation evaluator seam and composes
it with ADRs 0020 and 0021. It advances the milestone 3 execution gate but does
not complete authenticated session establishment or milestone 4 transport.
