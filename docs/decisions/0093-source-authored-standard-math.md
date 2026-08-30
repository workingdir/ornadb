# ADR 0093: Source-Authored Standard Math

**Status:** Accepted design direction

This ADR records an accepted design direction only. The current checkout retains
the standard-library chain through V9; no V10 source unit, exports, or math test
suite is claimed as implemented here.

## Decision

The proposed `orna.std/10` would append `stdlib/std/math.orna` to the accepted
standard-library source bundle. The unit would define three pure CLIENT
functions:

* `std.math.increment(p_value INTEGER) RETURNS INTEGER`
* `std.math.decrement(p_value INTEGER) RETURNS INTEGER`
* `std.math.is_zero(p_value INTEGER) RETURNS BOOLEAN`

The function bodies would be ordinary Orna expressions. The compiler would
lower their arithmetic and comparison expressions to the version-10 CLIENT
control-flow artifact. If implemented, the standard source checker, retained
snapshot, upgrade preparation, and client evaluator would use the same durable
function, parameter, revision, reference, and artifact identities.

If implemented, the V10 source unit would be an append-only child of V9.
Earlier standard snapshots would remain immutable. The source unit, catalogue
revision, source revision, and function revisions would have fixed identities
and canonical digest checks.

The CLIENT execution-fuel and call-depth limits remain host safety controls.
They do not change the source language model. `std.math` is pure and does not
perform database, filesystem, process, network, or runtime operations.

## Rationale

A standard function should be readable and changeable as `.orna` source, not
only represented by a Rust intrinsic. A small arithmetic family would give
application source a useful standard dependency while exercising the existing
typed CLIENT expression and control-flow path. A future application fixture
would use all three functions inside a `WHILE` loop and run through source
checking, preparation, authorisation, and evaluation.

This slice does not accept SERVER procedural bodies, `FOR` loops, general
collection semantics, exception tails, or unbounded host execution. Those are
separate language and execution contracts.

## Alternatives considered

### Change the V9 source unit

Rejected. V9 source bytes and identities are accepted historical state. Editing
them would invalidate the append-only upgrade chain and make old revisions
unrecoverable.

### Add Rust-only `std.math` intrinsics

Rejected. Rust remains appropriate for trusted kernel, codec, security,
storage, and host-runtime boundaries. Pure arithmetic helpers are language
code and must prove that the source path is real.

### Add SERVER procedural execution in this revision

Rejected for this slice. SERVER procedural execution needs a separate SQL
statement plan, transaction, audit, and PostgreSQL execution contract. Mixing
it with standard-source versioning would make the change hard to verify.

## Proposed evidence

The following evidence is required before this design can be promoted to an
implemented slice; these paths are not present in the current checkout:

* a retained `stdlib/std/math.orna` source unit and V10 standard export;
* compiler coverage for the canonical CLIENT control-flow executable records;
* an adapter/standard test that verifies retained source and artifact shape;
* an application fixture proving the normal offline check, prepare, authorise,
  and evaluate path.
