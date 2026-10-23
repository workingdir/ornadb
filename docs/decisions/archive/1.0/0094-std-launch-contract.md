# ADR 0094: Reference-Defined Run Entry Contract

**Status:** Accepted bounded `orna run` contract; `std.launch` metadata deferred

## Decision

OrnaDB 1.0.0 accepts only the launch behavior already defined by the frozen
reference:

* **Source and catalogue:** `std` is ordinary, explicitly imported library
  code pinned to a dependency snapshot. An absent optional module produces the
  ordinary import diagnostic. A module name alone does not define a portable
  API; its exact public declarations must come from its pinned source and
  `sys`. The reference defines no `std.launch` module, declarations, catalogue
  identity, metadata record, or metadata fields. No host-provided fallback may
  fill that gap.
* **Invocation:** `orna run <qualified-function>` invokes that named public
  function. Without a function name, `orna run` invokes root `main()` when
  present and otherwise produces a diagnostic. A file's path or name does not
  give it special execution behavior. These rules do not define additional
  argument-binding or target-resolution diagnostics.
* **Presentation:** Launch does not select or override a presenter. Existing
  presentation selection remains explicit presenter, session/page override,
  type `Present`, type `Display`, then derived Inspect; presenter failures use
  the defined Inspect fallback.
* **Runtime selection and security:** This contract selects no runtime,
  provider, package path, or alternate trust policy. Local commands retain the
  reference's invoking-OS-user trust boundary. The reference defines no
  per-application permissions, grants, or launch authorization metadata.
* **Lifecycle:** A run is a separately launched program with its own root owner,
  distinct from the hosting process. Finite programs exit when complete;
  unbounded stream programs remain active until cancellation or source close.
  Graceful REPL close cancels session-owned invocations and watches without
  terminating independent runs. No other launch, background, restart, or
  shutdown lifecycle is added here.

Consequently, `std.launch` declarations and metadata, their identities and
fields, application argument schemas, presenter/runtime selection through
metadata, launch-specific authorization, and any additional lifecycle or error
behavior are undefined by the OrnaDB 1.0.0 reference and remain out of scope.
This decision does not reserve names or compatibility behavior for them. A
future contract requires an authoritative specification change before those
semantics can be implemented as portable 1.0 behavior.

## Context

The issue's proposed metadata dimensions cannot be filled from a module name or
status document. `ORNA-LIB-001` through `ORNA-LIB-003` require snapshot-pinned
library behavior, reject undocumented host substitutes, and require exact
module declarations. The reference instead specifies a small CLI entry
contract in `source/06-expressions.md`, run ownership in
`source/10-execution.md`, and presenter selection independently in
`source/13-presentation.md`.

The conformance index lists `ORNA-RUN-001` through `ORNA-RUN-003` as planned
implementation obligations, not executed tests. The reference examples show
qualified `orna run` calls, but do not define `std.launch` metadata.

## Authority and traceability

This decision crosswalks, but does not extend or amend, these frozen reference
requirements and sections:

* `source/06-expressions.md`: `ORNA-RUN-001` through `ORNA-RUN-003`.
* `source/09-standard-library.md`: `ORNA-LIB-001` through `ORNA-LIB-003`.
* `source/10-execution.md`: run ownership and `ORNA-TASK-002`.
* `source/13-presentation.md`: presenter selection order and `ORNA-PRES-009`.
* `source/26-security.md`: `ORNA-TRUST-001`.
* `tests/requirements.json` and `tests/requirement-evidence.json`: normative
  requirement index and conformance status; the three run requirements are
  marked `not executed` / `planned` there.
* `examples/reference/README.md`: examples of qualified `orna run` targets.

**Precedence:** the frozen OrnaDB 1.0.0 reference is authoritative. This work
ADR records the bounded contract the implementation must follow and the
undefined behavior that remains out of scope; it adds no normative language.
