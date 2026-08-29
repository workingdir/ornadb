# ADR 0093: Source-Authored Standard Math

**Status:** Accepted design direction

## Decision

`orna.std/10` appends `stdlib/std/math.orna` to the accepted standard-library
source bundle. The unit defines three pure CLIENT functions:

* `std.math.increment(p_value INTEGER) RETURNS INTEGER`
* `std.math.decrement(p_value INTEGER) RETURNS INTEGER`
* `std.math.is_zero(p_value INTEGER) RETURNS BOOLEAN`

The function bodies are ordinary Orna expressions. The compiler lowers their
arithmetic and comparison expressions to the version-10 CLIENT control-flow
artifact. The standard source checker, retained snapshot, upgrade preparation,
and client evaluator use the same durable function, parameter, revision,
reference, and artifact identities.

The V10 source unit is an append-only child of V9. Earlier standard snapshots
remain immutable. The source unit, catalogue revision, source revision, and
function revisions have fixed identities and canonical digest checks.

The CLIENT execution-fuel and call-depth limits remain host safety controls.
They do not change the source language model. `std.math` is pure and does not
perform database, filesystem, process, network, or runtime operations.

## Rationale

A standard function must be readable and changeable as `.orna` source, not only
represented by a Rust intrinsic. A small arithmetic family gives application
source a useful standard dependency while exercising the existing typed CLIENT
expression and control-flow path. An application fixture uses all three
functions inside a `WHILE` loop and runs through source checking, preparation,
authorisation, and evaluation.

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

## Evidence

* `stdlib/std/math.orna` is the retained source unit.
* `crates/orna-compiler/src/resolver.rs` checks the source and builds the
  canonical CLIENT control-flow executable records.
* `crates/orna-standard/src/lib.rs` retains, verifies, and prepares V10.
* `crates/orna-server/tests/fixtures/std_math_dogfood.orna` uses the standard
  functions from ordinary Orna control flow.
* `crates/orna-standard/tests/v10_math.rs` verifies the retained source and
  artifact shape.
* `crates/orna-server/tests/v10_math_dogfood.rs` proves the normal offline
  check, prepare, authorise, and evaluate path.
