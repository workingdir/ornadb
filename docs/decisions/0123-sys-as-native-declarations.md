# ADR 0123: The `sys` surface is declared in Orna

**Status:** Accepted (2026-11-11, owner delegation). Records D19 in [DECIDED.md](../language/DECIDED.md).

**Supersedes in part:** [ADR 0111](0111-sys-baked-module-abi.md) (the authored inventory) and
[ADR 0112](0112-sys-provider-protocol-phase-1.md) (the `#[ornasys]` collector).

**Design:** [sys-as-native.md](../design/sys-as-native.md). It holds the inventory, the options and the evidence.

## Decision

The owner rejected a JSON schema as the definition of `sys` and asked for a native form.

1. `sys.orna` and its submodules are the one hand-written source of the `sys` surface. A new crate,
   `orna-sys-interface`, holds the files and exposes them as embedded strings, with a parse-checked test. It sits at
   layer 2, so `orna-hir`, `orna-sys-id` and `orna-check` can depend on it from above.
2. JSON is exporter output only. An exporter in `orna-sys-codegen` writes it from the declarations. A CI job pins the
   1.1.0 profile to the frozen `api/sys.json` byte for byte.
3. There is no `engine.orna`. The engine catalogue is split by member. Engine-only functions and failure codes become
   declarations under `sys.engine`. Sealed ids stay data with the archive until the archive decision is made.
4. The module layout follows the spec namespaces (`sys`, `sys.admin`, `sys.blob`, and the others). A `spelling` clause
   carries the 1.1 label for the 38 names that a path cannot give.
5. The declaration forms are `native fn` with clauses, `native type`, `native static`, and doc comments that the lexer
   keeps. Effect classes are true classes, with a `class` clause for the catalogue class (`read`, `invoke`, `admin`).
   Lifecycle is `since`, `removed`, `formerly` and `deprecated`. Prose is five fixed doc-comment sections: contract,
   preconditions, ownership, snapshot rule and invariants.
6. Failure codes are an enum with `__` as the segment break. Typed ids are a small generated set: failure codes and
   callable keys. Hosts register by qualified name at link. A build-time test checks that every declared callable is
   implemented or on a pinned not-yet list. No hot path looks up a string.
7. A deprecated overload spelling resolves as an alias to the one declaration that its argument type selects, with a
   deprecation warning (D5).

## What stands from ADR 0111 and ADR 0112

The provider protocol, the role contract, the closed effect vocabulary, the failure vocabularies, the ABI versions and
phases 2 to 6 of ADR 0111 stand. A role table keyed by declaration path and the declarations now produce what method
attributes produced.

## Consequences

* Staging is SN1 to SN8 in the design. The old pipeline keeps working until H9. The first proof is byte-equal JSON.
* The `#[ornasys]` attribute, the doc-section collector and `system_api_inventory.json` go at SN4.
* The checker's `host_effect` table goes when `orna check` moves to HIR (H3).
* `scripts/gen_interface.py` is rewritten at SN2 and deleted at SN3.
* `docs/design/layers.toml` gains the layer-2 entry for `orna-sys-interface`, and `allowed` entries for its dev
  dependency on `orna-syntax` and for the exporter in `orna-sys-codegen`, in the SN2 PR.
