# Work ADR 0118: Format-3 Schema Expression Digests

**Status:** Accepted

## Decision

The closed storage schema descriptor records two kinds of source expression
by digest: a computed field's fallback `[2, expression_digest]` and a refined
type's `assertion_semantic_digests` (Orna 1.1.0 `source/29-formats.md`,
"Closed storage schema descriptor"). The specification fixes neither the
digest domain nor its payload, and neither domain is one of the fixed domains
in that chapter. OrnaDB uses these implementation-owned domains:

| Expression | Domain |
|---|---|
| Computed field selector | `ornadb.computed-selector.v2` |
| Refined type assertion | `ornadb.refinement-assertion.v2` |

Each digest is `SHA256(domain || 00 || payload)`, where `payload` is the exact
OVB-1 encoding, as for the fixed domains of `source/29-formats.md`.

The payload is an array with one entry per lexical token of the expression,
in source order. The end-of-input token is not included.

* `["declaration", namespace, name]` for an identifier that is a free
  reference to a source declaration. It is resolved through the declaring
  module's own declarations and then its imports.
* `["module", namespace]` for an identifier that names an imported module
  (`use a.b;` or `use a.b as x;`).
* `["identifier", name]` for an identifier that is a row field, a local
  binding or a core name. `name` is the NFC-normalized spelling.
* `[class, spelling]` for every other token. Keywords, punctuation and
  literals keep their written spelling; string literals keep their quotes and
  escapes.

Resolution follows the selector scope of ORNA-FIELD-009. A local binding
(lambda parameter, `let`, `for` or `case` pattern) shadows a row field, and a
row field shadows module declarations and imports. Refinement assertions
have no row fields. Type names in annotations and constructor paths are
resolved as declarations. Member names after `.`, named-argument labels and
record-field labels keep their spelling, because the value's type resolves
them, not the scope.

## Rationale

* ORNA-SYS-034 excludes non-semantic formatting from semantic identity, so
  whitespace and comments are not part of the payload.
* `source/15-system.md` says semantic hashes include all observable
  declaration meaning. A name that resolves to a different declaration, for
  example after an import change, changes meaning without changing any token,
  so the payload records the resolved declaration rather than its spelling.
* The `v1` domains hashed spellings only. The resolved payload is a different
  payload, so it uses new `v2` domains instead of reusing the old ones.

## Consequences

* Renaming or moving a referenced declaration changes the digest of every
  selector or assertion that references it, and their schema nodes are
  restaged. A formatting-only edit leaves every digest unchanged.
* The digest identifies what an expression refers to, not the body of a
  referenced helper. A changed helper body leaves the digest unchanged.
* Postfix unit and currency suffixes (`5.km`, `12.34.GBP`) and member names
  keep their spelling and are not resolved.
