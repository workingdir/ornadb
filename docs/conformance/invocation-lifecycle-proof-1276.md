# Invocation lifecycle capture

This report captures the invocation behavior present at `origin/main` commit
`0624dc1cc17d21827e750b4f624ee496c970ae5f`. It records the local lifecycle
that exists today and identifies its boundary before portable row construction.
It does not add or imply multi-user authentication or authorization.

## Reference scope

**ORNA-TRUST-001** (`source/26-security.md:5`) excludes principals and grants
from Orna v1; the same chapter's public multi-user profile section
(`source/26-security.md:31-33`) excludes multi-user authentication and
authorization. No authenticated-principal behavior is claimed by this proof.

Invocation lifecycle is independently defined. **ORNA-SYS-056**
(`source/15-system.md:214`) requires an invocation identity before reflective
user code begins. **ORNA-SYS-057** (`source/15-system.md:216`) requires an
unhandled ordinary failure to mark the invocation failed before reporting it
as terminal. The canonical `sys.Invocation` row is specified at
`source/34-system-reference.md:1853-1884`; its fields include the function,
snapshot, owner links, arguments, start/end times, status, result type, failure,
trace, and idempotency-key hash.

## Behavior captured

In `crates/orna-sys-v1/src/lib.rs:1175-1234`, `Runtime::admit` validates and
binds the request, computes its invocation identity, allocates an invocation
ID, and stores the execution metadata before returning the execution boundary.
`Runtime::run` then calls admission before marking the invocation running and
calling the executor (`:1300-1322`). The local metadata projection maps a
retained ordinary failure to `Failed`, preserves its diagnostic, and records
an end time (`:1251-1298`).

The captured unit tests show that a revision change changes invocation
identity, a retained ordinary failure is observed as `Failed` with its
diagnostic, and invocation-ID exhaustion fails closed. The runtime projection
test confirms that a run's caller-supplied invocation ID is converted to a
checked `InvocationRef` using the run's admission-pinned capture.

## Where this stops

`orna-sys-v1::InvocationMetadata` is explicitly a local observation, not a
portable `sys.Invocation` row, and deliberately omits a durable `RowRef`,
diagnostic identity, parent/session link, and type witness until an owner
supplies the needed authority (`crates/orna-sys-v1/src/lib.rs:276-282`). Its
argument metadata is likewise local and carries no portable argument-row
reference (`:235-239`).

The runtime's `RunObservationRegistration` accepts a request, consumer,
function name, source identity, and invocation ID (`crates/orna-runtime-v1/src/lib.rs:1006-1014`).
`RunObservation::invocation_reference` validates reference coordinates only;
it expressly does not prove an invocation row is retained or grant authority
to observe one (`:1087-1102`). The runtime projection test therefore proves
the run-to-invocation coordinate link, not production of the canonical
`sys.Invocation` or `sys.InvocationArgument` relations.

Constructing those canonical rows remains the needs-data unblock tracked under
`ornadb-gov5.49.1`; this capture does not attempt it.

## Focused proof

Full captured CLI output, including compiler warnings and exit codes, is in
[`invocation-lifecycle-proof-1276.log`](invocation-lifecycle-proof-1276.log).
The Rust tests use no embedded Orna source, so no `.orna` fixture is involved.
