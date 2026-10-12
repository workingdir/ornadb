# Local capability gate demo evidence

Issue: `ornadb-1787968162880-334-ad711ea4` (GitHub #433)

## Bounded acceptance

The accepted demo is present on `origin/main` at
`crates/orna-client/examples/client_capability_demo.rs`. It creates one
`std.fs.read` path grant rooted at `/home/demo/project`, asserts that
`/home/demo/project/src/main.orna` is allowed, and asserts that
`/home/demo/project-other/src/main.orna` is denied at the component boundary.
It also shows literal and resolved-parameter declarations and denies an
unresolved parameter. The example uses the local Rust API only; it does not
load configuration, run an Orna program, or claim production filesystem
sandbox mediation. Its existing `just client-capability-demo` target runs the
same example.

This records the issue's requested boundary and the existing demo as evidence;
it does not claim that this local API is an OrnaDB 1.0 conformance feature.

## Frozen reference boundary

- `source/26-security.md:5`, **ORNA-TRUST-001**, places local commands inside
  the invoking OS user's trust boundary and says Orna v1 defines no grants or
  related principal/role system.
- `source/28-serving.md:19`, **ORNA-SERVE-005**, says core Orna defines no
  principals, groups, grants, per-device roles, or row-level permissions.
- `source/26-security.md:15`, **ORNA-EXT-001**, says a separate handwritten
  permissions manifest is not required merely to restate a component's WIT
  imports and exports. It does not define the local Rust grant API shown by
  this demo.

Search evidence against the frozen reference at
`/home/pbox/dev/ornadb/reference/Orna-1.0.0`:

```text
$ rg -n -i 'LocalCapabilityGrant|capability grant|capability gate|local grant|grant configuration|path grant|component-wise' source grammar tests examples api
[no output]
exit code: 1

$ rg -n -i 'capability' source
source/30-protocol.md:15: ... The session UUID alone is not a capability. ...
exit code: 0
```

The reference defines no local grant-set, path matching, or local gate-demo
contract. Configuration loading remains deferred as recorded in
`docs/decisions/0099-client-capability-grant-configuration.md`. The example's
allow/deny assertions are the issue's accepted local API demonstration, not a
new normative behavior inferred from the frozen reference.

## Focused execution evidence

Commands run from the repository root on the clean `origin/main` branch:

```text
$ cargo run --locked --offline -p orna-client --example client_capability_demo
local grant matching: literal declaration allowed
local grant matching: parameter declaration allowed
local grant matching: unresolved parameter denied
local grant matching: child path allowed (/home/demo/project/src/main.orna)
local grant matching: sibling path denied (/home/demo/project-other/src/main.orna)
exit code: 0

$ cargo test --locked --offline -p orna-client
test capability::tests::path_scope_covers_exact_and_subpath_values ... ok
test capability::tests::path_scope_rejects_siblings_parents_escapes_and_relative_values ... ok
test capability::tests::satisfies_declaration_resolves_parameter_references_at_the_gate ... ok
test execution::tests::capabilities::capability_gate_admits_a_granted_declared_capability ... ok
test execution::tests::capabilities::capability_gate_denies_an_ungranted_declared_capability ... ok
test result: ok. 425 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.35s
... remaining test binaries: 44 passed; 0 failed; doc-tests: 0 passed
exit code: 0
```

The package test command completed successfully (469 tests passed across test
binaries, zero failed). No tests or Orna source fixtures were added in this
evidence-only change.
