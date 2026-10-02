# External reference record: sys binding architecture addendum

**Status:** Reference addendum complete; this file records its provenance and follow-on implementation decisions.

**Beads issue:** `ornadb-btcjc`; GitHub issue [#5487](https://github.com/workingdir/ornadb/issues/5487).

## Recorded change

Added the standalone, explicitly non-normative reference addendum at:

`/home/pbox/dev/ornadb/reference/Orna-1.0.0/addenda/ADDENDUM-sys-bindings.md`

SHA-256: `c5e99a819782de1dd25e75ba7918834185f84bf959aee1b4b305a5494da94083`.

The reference directory is not a Git repository. The required `git -C /home/pbox/dev/ornadb/reference/Orna-1.0.0 status --short --branch` command returned “fatal: not a git repository (or any of the parent directories): .git” (exit code 128). No reference-tree VCS commit or push is possible. This repository change tracks the external documentation update; it does not contain or publish a copy of that file.

The addendum is a new file only. No chapter pointer was needed, so no hashed source or publication file was edited. It documents the distinction between the normative `api/sys.json` schema and its implementation provenance, notes that current merged code spells the operation annotation `#[ornasys]` while `#[sys_op]` is the requested architectural label, and keeps the WIT/Wasm extension boundary separate from built-in native `sys`.

## Verification record

### Beads link

Command: `env GITHUB_TOKEN="$(gh auth token)" BEADS_DIR=/home/pbox/dev/ornadb/.beads /home/pbox/.local/bin/bd github sync --push-only --issues ornadb-btcjc --verbose`

Captured output: `✓ Pushed 1 issues`. Exit code: `0`.

Command: `env BEADS_DIR=/home/pbox/dev/ornadb/.beads /home/pbox/.local/bin/bd show ornadb-btcjc --long --json`

Verified `external_ref=https://github.com/workingdir/ornadb/issues/5487`.

### Release checksums

Before and after adding the new file, the command `sha256sum release.json SHA256SUMS file-manifest.json source/manifest.json` returned the same values:

| Existing file | SHA-256 before | SHA-256 after |
|---|---|---|
| `release.json` | `f0f3ee5908d8ce2edef2bc14ca908cdea859f01b3dea27511206c2414a73d6e0` | `f0f3ee5908d8ce2edef2bc14ca908cdea859f01b3dea27511206c2414a73d6e0` |
| `SHA256SUMS` | `2152949a169e2e3494283db4aee7448ea6ecfef788789c998cda288e50b50315` | `2152949a169e2e3494283db4aee7448ea6ecfef788789c998cda288e50b50315` |
| `file-manifest.json` | `2c7844d1fc47681f47bb94ae811dd7c529cb224510fb562fe57759d14c05af3e` | `2c7844d1fc47681f47bb94ae811dd7c529cb224510fb562fe57759d14c05af3e` |
| `source/manifest.json` | `7b6fc36817360c24259bbd2eead054f3aa0af907ffb4d1d3a8d07b27232784b1` | `7b6fc36817360c24259bbd2eead054f3aa0af907ffb4d1d3a8d07b27232784b1` |

Command: `sha256sum --check --quiet SHA256SUMS`, run from the reference root. Captured output was empty; exit code: `0`.

The same manifest check confirmed `api/sys.json` at its existing SHA-256, `318b6d54f51d44e8117ffc91520dfd2dc2722cc023fdad51bd4dba601dd9abcb`. Its bytes and schema were not edited. The addendum is outside the existing release inventory; no checksum drift occurred among the existing release artifacts.

No Rust tests were run; this slice changes documentation only.

## Follow-on implementation note: registry emission (issue #5479)

The build generator treats `api/sys.json` only as the published compatibility artifact. Its input is the sys crate's internal type-graph inventory plus operation descriptors collected from annotated implementation methods. The inventory contains no function rows or counts; the method registry supplies all function rows, and the generator derives counts. The same collected operation registry feeds provider ABI and `.orna` stub generation.

The generator validates the completed document with the existing ORNA-SYS-129/138 type-graph checks and verifies that the unchanged published JSON Schema covers its exact root shape. Canonically ordered output remains byte-compared against the checked-in `api/sys.json`; this generation-flow change does not change the artifact path, schema, or 1.0.0 semantics.

## Follow-on implementation note: generated dispatch consumption (issue #5478)

The generated typed operation registry is also the runtime dispatch boundary, exposed as `system_dispatch_table()`. The older `system_provider_abi()` accessor forwards to the same immutable table for compatibility; it is not a second descriptor source. Semantic admission parses generated `api/sys.json` and validates each function against a typed registry lookup, including signature, effects, preconditions, and operation failure vocabulary. Runtime role linking and operation dispatch use the same table directly.

At dispatch, the runtime resolves the operation ID, checks each registry-declared precondition before invoking the native handler, and validates any returned failure against that operation's registered vocabulary. A failed precondition prevents handler invocation. The focused proof covers 1:1 mapping between generated API functions and typed registry operations, successful typed lookup/dispatch, precondition rejection, and undeclared failure rejection. This consumption path does not alter `api/sys.json`, the published schema, its normative artifact path, or 1.0.0 behavior.

## Follow-on implementation note: registry parity guard (issue #5623)

The build and focused CI proof share one artifact projection over the annotated Rust implementation registry plus the non-operation type-graph inventory. The proof reruns generation and byte-compares the normative `api/sys.json`, embedded dispatch JSON, bundled `.orna` declarations, and each generated module file against the build outputs; it also parses the regenerated dispatch table and compares it to the runtime typed table. The separate stub proof continues to check supported Orna grammar, resolved types, and one-to-one dispatch markers. These checks make all three artifacts standing registry projections without changing the frozen 1.0.0 contract.

## Follow-on proof note: generic keyword alias tail (issue #5639)

The parity proof also pins the generated `sys.invoke<T>` declaration whose type-witness parameter is named `as` in the registry and emitted as `as_` for Orna grammar. Its small `.orna` golden is stored inside `orna-sys-v1/tests/fixtures/` and included at compile time. This closes the focused reserved-keyword/generic tail without reading fixtures from the external reference tree or changing generated artifact bytes.

## Follow-on proof note: generic start keyword alias (issue #5655)

A sibling fixture pins `sys.start<T>` through the same keyword alias path, including the separate-transaction default and `sys.InvocationHandle<T>` result. The proof checks that the generated declaration parses, retains the registry operation's invoke role, type-witness alias, default, and result type, and appears byte-for-byte in the generated bundle. This extends syntax coverage without adding a second artifact source.

## Follow-on proof note: generic start overload alias parity (issue #5665)

An in-crate fixture now pins the erased `sys.start(Value)` and generic `sys.start<T>` declarations together. It proves both overload dispatch markers remain present in generated order and only the generic overload aliases the reserved registry parameter `as` to grammar-valid `as_`; both parsed result types still resolve to their respective registry contracts. The fixture remains a local compile-time include and adds no generated artifact or API change.

## Follow-on proof note: generic invoke overload alias parity (issue #5679)

A matching in-crate fixture pins the erased `sys.invoke(Value)` and generic `sys.invoke<T>` declarations together. Its parse and registry lookup checks preserve the overload order, `as` to `as_` alias only on the generic operation, and the two distinct registered result types, without changing the generated API artifacts.

## Follow-on proof note: invoke keyword parameter and default parity (issue #5775)

The invoke overload fixture proof now compares each parsed parameter name, type, and default with its typed registry entry. This pins the argument layout around the generic `as_` alias, including the `at`, transaction, and idempotency defaults for both overloads; generated artifacts remain unchanged.

## Follow-on coverage audit and extension-boundary parity (issue #6869)

The generated built-in host-binding registry currently contains 20 operations. The evaluator coverage audit maps every registry name to an in-crate `.orna` call fixture, its provider, and a native behavior proof with a concrete result assertion. It fails if an operation is added to or removed from the generated registry without updating that map. The behavior proofs cover:

| Registry operations | Native behavior proof |
|---|---|
| `std.io.environment.get`, `std.io.environment.require` | `typed_sys_environment_bindings_execute_native_allowlisted_provider` returns the explicit snapshot values `fixture-home` and `fixture-path`. |
| `std.io.process.run`, `std.concurrent.sleep` | `typed_sys_process_and_clock_bindings_execute_allowlisted_native_providers` returns the child output `host-bound` and observes the bounded clock wait. |
| `std.io.fs.read_text`, `write_text`, `list`, `metadata` | `sys_filesystem_registry_dispatches_real_reads_writes_lists_and_denials` reads and writes host files and checks the returned listing and metadata. |
| `std.io.fs.append_text`, `exists`, `is_directory`, `create_dir`, `copy_file`, `move_file`, `remove_file` | `all_registered_filesystem_dispatch_arms_execute_with_native_results` executes these fixtures and checks returned values and resulting filesystem state. |
| `std.io.fs.symlink_metadata` | `sys_filesystem_symlink_metadata_is_non_following_and_reads_cannot_escape_root` returns symlink metadata without following the link. |
| `std.net.http.send` | `sys_http_registry_dispatches_real_bounded_loopback_response` checks the loopback response status, header, and body. |
| `std.net.http.start`, `std.net.http.wait`, `std.net.http.cancel` | `sys_http_start_wait_and_cancel_dispatch_real_native_behavior` checks the asynchronous response body and cancellation result. |

This is coverage evidence for the built-in provider path, not a second descriptor inventory: operation identity, provider, role, signature, and failures still originate in the annotated Rust registry. The existing sys-crate proofs separately validate generated schema/artifact parity and parse/type/dispatch parity for generated declarations.

The implementation boundary matches the non-normative reference addendum and ADR 0111: built-in `sys` declarations and host dispatch are generated from the typed Rust registry and execute trusted native provider logic. WIT and the WebAssembly Component Model remain the distinct portable third-party extension ABI under ORNA-EXT-001..003. They provide typed, capability-scoped imports and exports without raw Turso or Git handles; they are not the built-in binding format, provider loader, or source of built-in `sys` behavior. No WIT component loader or extension permission is implied by this coverage work. The normative 1.0.0 contract and `api/sys.json` requirements are unchanged.

## Follow-on evaluator dispatch parity harness (issue #6883)

The evaluator unit proof now walks every generated host-operation descriptor through `SysHostBindingRegistry` with the native provider families installed. It asserts that registered non-environment operations reach their typed handler and fail at argument arity, rather than being treated as unknown or unbound; environment operations return their concrete optional/required snapshot values. Paired with the fixture audit above, this guards the chain from generated call surface through registry metadata to evaluator dispatch. The sys-crate generated-stub tests continue to pin parser, type, and public sys-dispatch parity. This is regression evidence only: built-in calls stay native, and WIT/Wasm remains a separate third-party extension boundary.
