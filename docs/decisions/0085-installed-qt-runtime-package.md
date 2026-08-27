# Work ADR 0085: Install and Select the Qt Runtime from a Fixed Package Path

**Status:** Accepted

## Canonical decision

Spec ADR 0021 accepts a separate `orna-runtime-qt` Debian package and the
fixed Linux x86_64 path `/usr/lib/orna/liborna-runtime-qt.so`. The local client
may use the explicit-path loader for development and smoke tests, but
production selection must use the fixed package-owned path only.

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
