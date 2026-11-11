# ADR 0123: The `sys` surface is declared in Orna, and no API definition is JSON

**Status:** Accepted (2026-11-11, owner delegation, amended the same day by the owner's correction). Records D19 in
[DECIDED.md](../language/DECIDED.md).

**Supersedes in part:** [ADR 0111](0111-sys-baked-module-abi.md) (the authored inventory and the published
`api/sys.json` projection) and [ADR 0112](0112-sys-provider-protocol-phase-1.md) (the `#[ornasys]` collector).

**Design:** [sys-as-native.md](../design/sys-as-native.md). It holds the inventory, the options and the evidence.

## Decision

The owner rejected a JSON schema as the definition of `sys` and asked for a native form. The owner then corrected the
first decision: "no json for sys please. i do not want json for any of the api definitions please." JSON is not even a
generated output.

1. `sys.orna` and its submodules, and `core/*.orna`, are the one hand-written source of the API. A new crate,
   `orna-sys-interface`, holds the `sys` files and exposes them as embedded strings, with a parse-checked test. It sits
   at layer 2, so `orna-hir`, `orna-sys-id` and `orna-check` can depend on it from above.
2. No API definition is JSON. The catalogues `sys-1.1.0.json`, `engine-1.1.0.json` and `std_intrinsics.json`, their
   copies, the authored inventory and its schema, and the JSON files that `orna-sys/build.rs` writes are deleted. No
   exporter writes JSON. Every consumer reads the declarations: the identity build, the old checker through an adapter
   over `orna-syntax` until H3, `orna-sys`, the evaluator bindings, `orna-project`, `orna-traceability` and the
   conformance drift tests. A tool that needs a data format builds it from the lowered declarations when it runs.
3. The proof is equality of models, not of bytes. A test builds the typed model of the declarations and the typed model
   of the JSON that the code reads today, and prints every difference. When it prints none, each consumer flips, and
   then the JSON and every drift check that guarded it are deleted.
4. The frozen 1.1.0 publication stays as history. `reference/Orna-1.1.0` is checkout-local, and the fixture trees keep
   hash-pinned copies. Nothing in the build reads them as an API definition. One conformance test compares the
   declarations of profile 1.1.0 with the fixture copy, model against model.
5. There is no `engine.orna`. The engine catalogue is split by member. Engine-only functions and failure codes become
   declarations under `sys.engine`. The sealed ids and the host operations become Rust tables, never JSON.
6. The module layout follows the spec namespaces (`sys`, `sys.admin`, `sys.blob`, and the others). A `spelling` clause
   carries the 1.1 label for the 38 names that a path cannot give.
7. The declaration forms are `native fn` with clauses, `native type`, `native static`, and doc comments that the lexer
   keeps. Effect classes are true classes, with a `class` clause for the catalogue class (`read`, `invoke`, `admin`).
   Lifecycle is `since`, `removed`, `formerly` and `deprecated`. Prose is five fixed doc-comment sections: contract,
   preconditions, ownership, snapshot rule and invariants.
8. Failure codes are an enum with `__` as the segment break. Typed ids are a small generated set of Rust names: failure
   codes and callable keys. Hosts register by qualified name at link. A build-time test checks that every declared
   callable is implemented or on a pinned not-yet list. No hot path looks up a string.
9. A deprecated overload spelling resolves as an alias to the one declaration that its argument type selects, with a
   deprecation warning (D5).

## What stands from ADR 0111 and ADR 0112

The provider protocol, the role contract, the closed effect vocabulary, the failure vocabularies, the ABI versions and
phases 2 to 6 of ADR 0111 stand. A role table keyed by declaration path and the declarations now produce what method
attributes produced. The registry no longer emits `api/sys.json`. The frozen artifact is the specification's own
publication, and this repository keeps it as history.

## Consequences

* Staging is SN1 to SN8 in the design. The old pipeline keeps working until H9. SN1 ends with the equality proof, SN2
  and SN3 flip the consumers, and SN4 deletes the JSON.
* The `#[ornasys]` attribute, the doc-section collector, `system_api_inventory.json` and the other JSON files go at
  SN4.
* The checker's `host_effect` table goes when `orna check` moves to HIR (H3), and `system_api.rs` goes with it.
* `scripts/gen_interface.py` and `scripts/gen_core_interface.py` are extended once and deleted in SN1.
* `docs/design/layers.toml` gains the layer-2 entry for `orna-sys-interface` and one `allowed` entry for its dev
  dependency on `orna-syntax`, in the SN1 PR.
