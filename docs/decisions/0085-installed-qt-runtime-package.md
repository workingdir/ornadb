# Work ADR 0085: Install and Select the Qt Runtime from a Fixed Package Path

**Status:** Deferred; not accepted as an OrnaDB 1.0.0 contract

## OrnaDB 1.0.0 normative disposition

The frozen OrnaDB 1.0.0 reference does not define an installed Qt runtime
package contract. `Orna-1.0.0.md` §1 explicitly says the specification does
not prescribe a user-interface toolkit or hosting provider. The closest
verified requirement, `ORNA-SYS-006` in `source/15-system.md`, requires the
runtime to expose declared `sys.RuntimeInfo` coordinates; it does not define
package identity, installation path, package authority, discovery, or runtime
selection.

A targeted search of `Orna-1.0.0.md`, `source/`, `grammar/`, `tests/`,
`examples/`, `api/`, and `profiles/` for `installed Qt`, `Qt runtime`,
`runtime package`, `package identity`, `package authority`, `fixed path`,
`runtime offer`, `runtime selection`, `shared library`, and `Debian package`
found no normative installed-Qt packaging or selection contract. The only
related match was the `std` source-snapshot coordinate description in
`source/15-system.md`; it does not prescribe native runtime packaging.

Accordingly, the fixed Linux path, separate Debian package, package authority,
and deterministic discovery/selection below remain a historical work
proposal. They are deferred for OrnaDB 1.0.0 until a canonical normative
requirement accepts them. Do not implement or claim them as 1.0.0 behavior.

## Historical proposal (not normative)

This earlier proposal attributed a separate `orna-runtime-qt` Debian package
and the fixed Linux x86_64 path `/usr/lib/orna/liborna-runtime-qt.so` to Spec
ADR 0021. Those deployment choices do not appear in the frozen 1.0.0 reference
and are not accepted by the normative disposition above. The remaining
sections record the proposal for historical context only.

## Host boundary

`InvocationRuntimeOffer` remains pathless. The client validates the loaded
library descriptor before it constructs an offer or a `RuntimeSession`. The
server and database plan receive only the typed sink and contract facts. No
source value, database artifact, principal, or grant can select a path.

The installed invocation path selects Qt for a `std.ui.UI` result and TTY for
the accepted terminal sinks. An explicit runtime override is validated before
sealed request construction. A missing or incompatible Qt package fails closed
for a UI result and never falls back to a terminal renderer.

The host owns the caller-pumps `RuntimeSession` on the worker thread that
executes the CLIENT function. Existing resource, action, Inspector, security,
and protocol boundaries remain in the installed executor. The Qt adapter owns
only the `std.ui.window@1` external contract and delegates all other work to
the existing executor.

## Packaging

The main `orna` Debian package keeps its one-executable payload and does not
ship a shared library. The separate runtime package owns the Qt shared object
and its Qt dependencies. Package authentication remains the existing Debian
repository authority. The runtime CMake target has an install rule for the
fixed path; package assembly and clean-host verification remain separate
release evidence.

## Deferred

This ADR does not accept a second runtime family, a browser runtime, runtime
archives, database-selected native code, arbitrary production environment
paths, UI constructor functions beyond the accepted window contract, or
list/table model contracts.
