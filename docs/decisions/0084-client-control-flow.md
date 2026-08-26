# Work ADR 0084: Programmable CLIENT Plans and Shared Runtime Hosts

**Status:** Accepted

## Canonical decision

Spec ADR 0020 accepts the first programmable CLIENT core. This ADR records the
implementation split for the work repository. The canonical specification
controls syntax and language meaning; this file controls only the artifact and
host boundaries.

## Plan format

Keep client-plan versions 1 through 9 unchanged. Add one append-only plan
version for the control-flow subset. Its model contains typed expression nodes,
ordered local identities, statement blocks, IF branches, WHILE bodies, and
explicit RETURN statements. The decoder enforces depth, node, statement, body,
operand, and encoded-size limits before the evaluator receives the plan.

The new plan uses checked signed 64-bit arithmetic and strict BOOLEAN
conditions. The evaluator receives a per-root fuel counter. Fuel exhaustion,
invalid arithmetic, and invalid control flow map to stable redacted client
errors. No database value or runtime offer can change the limit.

## Compiler and evaluator

The syntax parser and compiler must retain exact source spans, resolve local and
parameter identities, preserve call/reference evidence, and lower the new
constructs without changing old artifact versions. The evaluator must execute
branches, loops, early returns, calls, resources, actions, state, and external
contracts through explicit state transitions. Recursive calls remain allowed
until the existing depth/fuel limits fail closed.

## Shared runtime host

`RuntimeLibrary` and `RuntimeSession` are client-owned infrastructure. A host
adapter may install them in any CLIENT executor, not only Studio. The host
selects an explicit trusted runtime offer outside database plans, presents UI
values through the selected runtime, and converts callbacks into owned event
snapshots. Console clients continue to use the existing TTY path.

The Qt Studio examples remain smoke/demo programs. They are not the runtime
integration boundary and must not become a second source of UI semantics.

## Deferred

This ADR does not accept collection/range `FOR`, general user-defined algebraic
value types, a second toolkit, browser deployment, populated Inspector rows,
launch metadata, gateway exposure, or a general UI transport beyond the
accepted `ORNA-UI/1` value and Qt v1 operation subset.
