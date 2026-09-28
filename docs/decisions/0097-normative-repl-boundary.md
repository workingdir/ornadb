# Work ADR 0097: Normative REPL Boundary

**Status:** Accepted implementation scope; function-backed session API deferred

## Decision

The local interactive session follows the frozen Orna 1.0.0 REPL contract:

* the REPL is an ephemeral module with ordinary `use` syntax and session-local
  `$_` and `$?` bindings (`ORNA-REPL-001`, `ORNA-REPL-002`);
* eager previews do not perform effects, and relation previews do not require
  complete enumeration (`ORNA-REPL-003`, `ORNA-REPL-004`);
* `:at` selects the session snapshot without changing repository `HEAD`
  (`ORNA-REPL-005`);
* `:watch` uses the page-session dependency and delta machinery, including
  resynchronization, and graceful close cancels the session-owned watch
  (`ORNA-REPL-006`, `ORNA-LIVE-001` through `ORNA-LIVE-004`,
  `ORNA-TASK-002`).

The function-backed CLI-session API described by work ADR 0092 is explicitly
deferred. Orna 1.0 defines `sys.invoke` as typed reflection over a resolved
function identity and prohibits it from evaluating arbitrary source
(`ORNA-SYS-077`). The frozen reference does not define a `std.cli.repl`
catalogue identity, a terminal-input effect, or a host window-creation
lifecycle. Those names and behaviors are not 1.0 APIs and this decision does
not reserve identities or semantics for them.

This deferral does not defer the normative `orna repl` behavior. That command
continues to be governed by the REPL and execution requirements in the frozen
reference. This ADR records implementation scope; it does not claim that any
REPL requirement has been fully executed or proven.

## Authority and traceability

The frozen Orna 1.0.0 reference controls this scope:

* `source/10-execution.md`: activation/session ownership and graceful REPL
  close (`ORNA-TASK-002`);
* `source/14-pages.md`: shared page watch and delta behavior
  (`ORNA-LIVE-001` through `ORNA-LIVE-004`);
* `source/15-system.md`: typed reflective invocation and source-evaluation
  boundary (`ORNA-SYS-077`);
* `source/17-repl.md`: the interactive-session model and
  `ORNA-REPL-001` through `ORNA-REPL-006`;
* `source/18-cli.md`: the `orna repl` command;
* `source/20-embedded.md`: in-process local ownership
  (`ORNA-EMBED-002`).

**Precedence:** the frozen OrnaDB 1.0.0 reference is authoritative. This work
ADR records the deferred portion of work ADR 0092 and does not add or modify
normative language.
