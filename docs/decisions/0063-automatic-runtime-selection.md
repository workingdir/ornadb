# ADR 0063: The Client Offers the Installed Runtime and Accepts a Runtime Override

**Status:** Accepted

## Decision

The `orna` client names its installed runtime(s) in the sealed `sys.invoke`
client offer, accepts an optional `--runtime <family>` override, and selects
the runtime deterministically. The wire contract already exists
(`InvocationRuntimeOffer`, `InvocationRuntimeContract`, `InvocationClientOffer`,
and the `canonical_runtime_list` / `append_client_offer` codec); this decision
makes the client actually populate and respect it.

Today, the only installed runtime is `orna-runtime-tty`, so automatic
selection defaults to `tty` and any override to a non-installed family fails
closed at the CLI with exit code 2. This slice does not implement platform
preference defaults, does not add a `std.ui.UI` sink offer, and does not emit
`RuntimeOfferAccepted`/`Rejected` events; those are deferred (see below).

## Background

Work ADR 0057's TTY runtime section established that the client offers the terminal sinks
and selects `orna-runtime-tty` deterministically. The sealed route carries the
client offer verbatim; the server never validates runtime offers, so populating
them cannot break the invoke path. Work ADR 0056's deferred `--runtime` section. The runtime-offer wire model has been complete and tested since the sealed `sys.invoke` carriers landed; only the client offer has
left the list empty (`build_sealed_request` in `crates/orna-server/src/invoke.rs`).

The spec CLI documents the advanced override shape (spec `docs/15-runtime-architecture.md`),
and the invariant that a database cannot order the client to load an arbitrary
shared library (spec `docs/13-invocation-system.md` section 8). Automatic
selection remains a local client decision.

## Surface

```text
orna --runtime tty invoke studio.main          # global override before the command
orna invoke std.invoke.echo                     # no option: deterministic default
```

- `--runtime <family>` is accepted both as a global option before the command
  and inside `orna invoke ... [options]`. An unknown family, a missing value,
  or a non-installed family is a usage error (exit 2).
- The only recognised family today is `tty`. Unknown families such as `qt`,
  `gtk`, `swiftui`, `imgui`, and `web` fail closed because no such runtime is
  installed in this workspace.

## Runtime offer

The client builds one `InvocationRuntimeOffer` for the installed tty runtime:

| Field | Value | Source |
| --- | --- | --- |
| name | `tty` | spec family name |
| version | `0.1.0` | workspace crate version of `orna-runtime-tty` |
| consumed descriptors | `std.terminal.Document`, `std.io.ByteStream` | the two sink types it renders |
| contracts | empty | no UI contract surface exists yet |
| preference rank | `0` | matches the sink offers default |
| trusted | `true` | the binary's own linked runtime |
| limits | `None` | no typed limits |

The identity lives with the runtime crate: `orna-runtime-tty` gains
`RUNTIME_NAME = "tty"` and `RUNTIME_VERSION = env!("CARGO_PKG_VERSION")`
constants so family identity is not duplicated in the server.

## Automatic selection policy

```rust
fn selected_runtime(request) -> Result<Option<RuntimeFamily>, InstalledInvokeError>
```

- No override: `Some(Tty)` (the only installed runtime).
- Override `tty`: `Some(Tty)`.
- Override any other family: `Usage` error (exit 2).

Platform preference defaults (Linux desktop gtk > qt > imgui) are a current
proposal in the spec and depend on local configuration; they are a later
slice and are deliberately not implemented here.

## Precedence and non-terminal stdout

This later decision is authoritative for the boundary between runtime
selection and sink consumption. It supersedes only the initial
stdout-is-a-terminal gate in ADR 0057's TTY runtime section: with TTY as the only installed
runtime, the deterministic default (and an explicit `--runtime tty`) selects
TTY for both `CliTty` and `CliPipe` caller contexts. Once TTY is selected, its
sink map is unconditional: `std.terminal.Document` and `std.io.ByteStream`
values are written to stdout even when stdout is redirected. `CliPipe` remains
an observation in caller context, not a second output format or a reason to
emit a typed envelope.

This does not choose a non-TTY runtime, add automatic machine-format
selection, or define runtime contracts/events. Platform preferences, a second
runtime, and any different policy for non-terminal output remain deferred to a
later accepted decision.

## Renderer seam

`select_runtime_sink` in `crates/orna-server/src/invoke.rs`
already maps the two standard opaque types to `orna_runtime_tty::Sink`. Its mapping is the tty family's sink map;
it stays unchanged while tty is the only runtime. When a second family lands,
the renderer gains the selected-family parameter. A comment marks the seam.

## Deferred (documented, not invented)

- `std.ui.UI` as a sink offer and contract-aware selection remain
  deferred. The value type and the `CREATE EXTERNAL CLIENT FUNCTION ...
  RUNTIME CONTRACT` syntax and closed expression path now exist (work ADRs
  0062 and 0068), but this slice does not define a graphical runtime offer or
  UI contract.
- Transitive-contract validation for a UI sink remains pending the accepted
  graphical runtime contract and its compatibility rules.
- `RuntimeOfferAccepted`/`Rejected` events: `InvocationEventBody` has no such
  variants and no emitter exists. Selection is a client decision; the visible
  surface for this slice is `--explain` naming the selected family.

## Consequences

- `InstalledInvokeRequest` gains a `runtime: Option<RuntimeFamily>` field
  (public, `#[non_exhaustive]`); `new` gains a positional parameter and all
  call sites update in one commit (clean cutover).
- `build_sealed_request` passes `installed_runtime_offers()` filtered by the
  selected family instead of `Vec::new()`; its doc comment is corrected.
- `render_explain` names the selected family instead of printing only a count.
- The parser change closes the friendly-argument hole where
  `orna invoke ... --runtime qt` silently became a parameter named `runtime`.
- Integration proof path: the existing Compose-Postgres proof is retained as a
  Compose-gated path to round-trip the tty offer through the sealed
  encode/dispatch/decode path; it is not a current local or live result.