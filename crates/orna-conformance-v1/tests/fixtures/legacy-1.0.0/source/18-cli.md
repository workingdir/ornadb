# 18. Command-line interface {#cli}

## Git-compatible commands

```text
orna init      clone      status     add        commit     log
orna diff      branch     checkout   switch     merge      reset
orna push      pull       fetch      remote     stash      tag
orna blame     show       restore    mv
```

**ORNA-CLI-001** If Git already has the operation, Orna SHOULD preserve its name, flags and observable semantics. `orna mv` additionally preserves Orna semantic identity when the target is a definition or row key.

## Orna-specific commands {#cli-small-orna-specific-surface}

The Orna-specific commands are:

```text
orna repl
orna run
orna serve
orna check
orna fmt
orna explain
orna verify
orna prune
```

There are no required `orna stream`, `orna checkpoint`, `orna failure`, `orna grants` or extension-permission command hierarchies.

**ORNA-CLI-002** Runtime/checkpoint/failure detail and uncommon recovery operations SHOULD be available through typed `sys` values in the REPL and optional `std.devtools` pages.

## Status

```text
$ orna status

On branch main
Your branch is ahead of 'origin/main' by 3 commits.

Changes to be committed:
  modified   energy/main.orna

Changes in CWD:
  modified   directory.Contact/alice-smith

Runtime data:
  sensors.greenhouse.Reading
    184,221 rows · 21.8 MiB · oldest 49s

Programs:
  messages.sync    blocked · invalid MIME · 12 attempts
  bank.sync           running · checkpoint page:92
```

**ORNA-CLI-003** `orna status --porcelain` MUST provide stable machine-readable output.

**ORNA-CLI-004** `orna status --short` SHOULD preserve Git's compact style while adding stable codes for runtime CWD summaries.

## Diagnostics

```text
error[E0312]: column `email` cannot become non-optional
  ┌─ contacts/main.orna:8:5
  │
8 │     email: Str,
  │     ^^^^^^^^^^ 14 existing rows contain null
  │
  = affected table: directory.Contact
  = required by: contacts.page()
  help: provide a default or update the existing rows first
```

**ORNA-DIAG-001** Errors MUST have stable codes, concise titles, source spans when available, causes and actionable help where known.

**ORNA-DIAG-002** `orna explain <code>` SHOULD provide extended documentation.

## Output controls

```text
-v / -vv
-q
--color auto|always|never
--format human|short|json
```

**ORNA-CLI-005** Dynamic progress MUST be suppressed when output is not a terminal unless explicitly forced.

**ORNA-CLI-006** Terminal hyperlinks SHOULD be emitted when supported.

## Visual language

```text
green   success / valid
yellow  warning / change
red     error / conflict
cyan    object names / refs / paths
dim     secondary metadata
```

No emoji-heavy celebration or decorative ASCII branding is required.


## User-facing diagnostics

**ORNA-UX-001** Normal CLI output, REPL output, diagnostics, generated user documentation and default frontend text MUST describe the problem and remedy using Orna concepts.

**ORNA-UX-002** Normal user-facing output MUST NOT require the user to understand internal profile names, storage libraries, embedded-database terminology, wire encodings, object formats or extension ABI terminology.

**ORNA-UX-003** `--verbose` MAY provide additional Orna-level context. Raw implementation details require `--debug` or an equivalent explicitly technical interface.

**ORNA-UX-004** A diagnostic MUST lead with the user-visible condition and an actionable remedy. An implementation MAY attach machine-readable technical causes without rendering them by default.
