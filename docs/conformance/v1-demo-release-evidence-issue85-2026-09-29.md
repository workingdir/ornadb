# V1 demo execution evidence and release boundary

Beads: `ornadb-1787960946377-8-63309470` (GitHub #85)

Audited repository base: `3d52bc7c9301f077eff33e37861e2d865dc97fab` (`origin/main`).
Frozen reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`.

## Scope

This record captures execution evidence for the three reproducible Rust API
examples wired into `just demo-suite`: the TTY renderer, client artifact
integrity check, and local capability matcher. They demonstrate those Rust
surfaces; they do not run an Orna source project or establish product-release
readiness. The local capability example already has its own detailed record in
[`local-capability-gate-demo-evidence-2026-09-28.md`](local-capability-gate-demo-evidence-2026-09-28.md).

The frozen reference describes a runnable worked Orna database at
`source/31-examples.md` and `examples/reference/`. It requires a complete
example project to include its required modules and schemas or pin an exact
interface (**ORNA-EVIDENCE-002**, `source/32-conformance.md:71`). The in-tree
Rust examples are not that Orna project, so this evidence does not claim they
satisfy that complete-project requirement. A release report must separate
authored cases from executed implementation evidence and must not infer a pass
from a filename or manifest (**ORNA-EVIDENCE-001**,
`source/32-conformance.md:69`); test specification, implementation, and passing
execution must likewise remain distinct (**ORNA-TEST-004**,
`source/32-conformance.md:23`).

Debian production packaging is out of scope here. PR #1885 records that the
frozen reference has no Debian packaging requirement and defers the missing
Debian release paths to a separate packaging slice. This document adds no
package files, release workflow, signed artifact, or publication claim.

## Captured execution

At the audited base, `just demo-suite` ran all three configured commands and
exited 0. The direct commands below were also run with locked, offline Cargo
resolution; the recorded text is stdout. Cargo emitted existing compiler
warnings on stderr for the client examples; all commands still exited 0.

```text
$ cargo run --quiet --locked --offline -p orna-runtime-tty --example runtime_demo
stdout bytes (hex): 72 75 6e 74 69 6d 65 5f 64 65 6d 6f 3a 20 74 65 72 6d 69 6e 61 6c 20 64 6f 63 75 6d 65 6e 74 0a 00 ff 01 fe
exit code: 0

$ cargo run --quiet --locked --offline -p orna-client --example client_artifact_demo
client artifact integrity: valid CLIENT plan accepted and decoded
client artifact integrity: SERVER payload rejected
client artifact integrity: digest mismatch rejected
exit code: 0

$ cargo run --quiet --locked --offline -p orna-client --example client_capability_demo
local grant matching: literal declaration allowed
local grant matching: parameter declaration allowed
local grant matching: unresolved parameter denied
local grant matching: child path allowed (/home/demo/project/src/main.orna)
local grant matching: sibling path denied (/home/demo/project-other/src/main.orna)
exit code: 0

$ just demo-suite
exit code: 0
```

The TTY stdout decodes to `runtime_demo: terminal document\n` followed by
bytes `00 ff 01 fe`; they are the byte-stream payload from the example, not
UTF-8 text. No Cargo tests were run for this evidence-only change. No Orna
source or `.orna` fixture was changed.

## Remaining boundary

This records runnable demo output only. It does not claim a live end-to-end
Orna database journey, a complete self-contained Orna example project, a
production package, or a release. Those claims require their own accepted
scope and evidence.
