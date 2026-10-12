# Rows inactive-type Clippy report deferral — issue #312

Progress and resolution evidence for Beads `ornadb-1787968147584-214-83c3b78f`, mapped to GitHub #312. Inspection used `origin/main` `70632b4cbc7f054e557b24426e6961597caa8a87`.

## Finding

The accepted issue says Rust 1.95 Clippy flags a nested inactive standard enum/record check in `validate_rows_value` and asks to collapse that branch while preserving fail-closed admission order. The current `crates/orna-protocol/src/rows.rs` has no inactive-type enum/record branch in `validate_rows_value` (lines 522–579). That function rejects unsupported runtime carriers and type mismatches, then delegates declared-type validation to `rows_type_wire`. The related nested enum/record admission code is in `validate_rows_declared_type` (lines 443–480), where it checks the active application and standard catalogues. No Clippy diagnostic for this Rows code was reproduced.

## Reference and test-path searches

The frozen Orna reference contains no `ORNA-ROWS` requirement and no requirement text for an inactive Rows type. These searches returned no matches:

```text
rg -n 'ORNA-ROWS' reference/source reference/api reference/tests/requirements.json reference/tests/requirement-evidence.json
rg -n -i 'inactive.{0,40}type|type.{0,40}active' reference/source reference/api reference/tests/requirements.json reference/tests/requirement-evidence.json
```

The nearest verified normative clause is **ORNA-PROTO-001**, `/home/pbox/dev/ornadb/reference/Orna-1.0.0/source/30-protocol.md:9`: protocol implementations must decode and validate the complete message, required fields, canonical forms, and configured limits before admitting an operation. It does not specify a Rows-specific inactive-type rule or Clippy/code shape.

Searches for `encode_rows(`, `decode_rows(`, and `InactiveType` under `crates/orna-protocol/src/tests.rs` and `crates/orna-protocol/src/tests/` found no Rows test call sites; the only `encode_rows` / `decode_rows` matches under `crates/orna-protocol/src` are the implementation definitions and the encode wrapper. Therefore the package regression run below is not a focused Rows inactive-type test.

## Executed checks

| Command | Result | Captured output |
|---|---|---|
| `cargo +1.95.0 clippy -p orna-protocol --all-targets -- -D clippy::collapsible_match` | exit 0; no Rows `collapsible_match` diagnostic | `/var/tmp/ornadb-rows-clippy-rust195-baseline.log` |
| `cargo +1.95.0 test -p orna-protocol --lib -- --nocapture` | 198 passed, 0 failed; exit 0 | `/var/tmp/ornadb-rows-protocol-lib-test.log` |
| `cargo +1.95.0 clippy -p orna-protocol --all-targets -- -D clippy::collapsible_match -D clippy::collapsible_if` | exit 101 due to three `collapsible_if` errors in dependency `orna-semantic-v1`, outside the Rows path | `/var/tmp/ornadb-rows-clippy-targeted.log` |

The stricter exploratory command's diagnostics are preserved verbatim:

```text
error: this `if` statement can be collapsed
    --> crates/orna-semantic-v1/src/semantic_payload.rs:1229:9
     |
1229 | /         if path.len() == 1 {
1230 | |             if let Some(Type::Record(fields)) = scope.nominal_rows.get(enum_or_nominal) {
1231 | |                 return Ok((Vec::new(), fields.clone()));
1232 | |             }
1233 | |         }
     | |_________^
     |
     = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.95.0/index.html#collapsible_if
     = note: requested on the command line with `-D clippy::collapsible-if`
help: collapse nested if block
     |
1229 ~         if path.len() == 1
1230 ~             && let Some(Type::Record(fields)) = scope.nominal_rows.get(enum_or_nominal) {
1231 |                 return Ok((Vec::new(), fields.clone()));
1232 ~             }
     |

error: this `if` statement can be collapsed
    --> crates/orna-semantic-v1/src/lib.rs:5865:9
     |
5865 | /         if let Some(symbol) = scope.names.get(text) {
5866 | |             if !symbol.generic_parameters.is_empty() {
5867 | |                 return Some(symbol.generic_parameters.clone());
5868 | |             }
5869 | |         }
     | |_________^
     |
     = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.95.0/index.html#collapsible_if
help: collapse nested if block
     |
5865 ~         if let Some(symbol) = scope.names.get(text)
5866 ~             && !symbol.generic_parameters.is_empty() {
5867 |                 return Some(symbol.generic_parameters.clone());
5868 ~             }
     |

error: this `if` statement can be collapsed
    --> crates/orna-semantic-v1/src/lib.rs:5930:5
     |
5930 | /     if let Some(module) = scope.available_modules.get(&direct_namespace) {
5931 | |         if let Some(symbol) = module.exports.get(parts[parts.len() - 1]) {
5932 | |             return Some(symbol);
5933 | |         }
5934 | |     }
     | |_____^
     = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.95.0/index.html#collapsible_if
help: collapse nested if block
     |
5930 ~     if let Some(module) = scope.available_modules.get(&direct_namespace)
5931 ~         && let Some(symbol) = module.exports.get(parts[parts.len() - 1]) {
5932 |             return Some(symbol);
5933 ~         }
     |
error: could not compile `orna-semantic-v1` (lib) due to 3 previous errors; 5 warnings emitted
cargo_clippy_exit=101
```

## Resolution

Defer the requested code change and close issue #312 as stale/non-reproducing at the current main base: the named branch is absent from the named function and the reported Clippy lint passes under Rust 1.95. No Rows behavior or code was changed. ORNA-PROTO-001 remains the only directly relevant verified reference clause found; this deferral does not claim conformance for untested Rows behavior. No Orna source input was added, so no fixture change was needed.
