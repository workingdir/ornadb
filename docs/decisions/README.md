# Architecture decision records

OrnaDB has two accepted ADR series. The number is unique only within its
series:

* **spec ADR NNNN** is a canonical specification decision in
  `spec/adrs/NNNN-*.md`.
* **work ADR NNNN** is an implementation decision in
  `work/docs/decisions/NNNN-*.md`.

Use the qualified form when referring to a decision: `spec ADR 0001` or
`work ADR 0001`. Within a work decision file, an unqualified `ADR NNNN` means
the work series.

Work ADRs narrow implementation choices for the canonical specification. If a
work ADR conflicts with the canonical specification, its explicit `Precedence`
section governs the conflict for the scope stated in that section. This index
defines naming and traceability only. It does not change the existing
source-of-truth or authority rules.

## Current work ADRs

* **work ADR 0001:** [PostgreSQL Is a Private Kernel](0001-private-postgresql-kernel.md)
* **work ADR 0002:** [Public Language Contract](0002-public-language-contract.md)
* **work ADR 0003:** [Active Source Revision Is Authoritative](0003-source-revision-authority.md)
* **work ADR 0004:** [Private PostgreSQL Layout Uses Stable Orna Identities](0004-private-postgresql-layout.md)
* **work ADR 0005:** [Single-Row SERVER INSERT](0005-single-row-server-insert.md)
* **work ADR 0006:** [Field Renames Are Replay-Safe Identity Transitions](0006-replay-safe-field-renames.md)
* **work ADR 0007:** [Single-Object SERVER UPDATE](0007-single-object-server-update.md)
* **work ADR 0008:** [Single-Object SERVER DELETE](0008-single-object-server-delete.md)
* **work ADR 0009:** [Identity-Selected SERVER SELECT](0009-identity-selected-server-select.md)
* **work ADR 0010:** [Parameter-Free SERVER SELECT DISTINCT](0010-parameter-free-server-select-distinct.md)
* **work ADR 0011:** [Direct Boolean SERVER SELECT Predicates](0011-direct-boolean-server-select-predicates.md)
* **work ADR 0012:** [Direct Boolean Predicates in SELECT DISTINCT](0012-direct-boolean-select-distinct-predicates.md)
* **work ADR 0013:** [Required Unique Reference Fields](0013-required-unique-reference-fields.md)
