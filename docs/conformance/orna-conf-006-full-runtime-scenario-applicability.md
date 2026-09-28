# ORNA-CONF-006 Full-Runtime Scenario Applicability

Epic: `ornadb-1787968123319-16-24513f57` (GitHub #89)
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`

## Normative criterion and scope

**ORNA-CONF-006** states that a full-runtime claim MUST execute the applicable
assertion, automatic-failure, recovery, conversion, transaction, cancellation,
and cross-table validation scenarios (`source/01-scope.md:60`). The Runtime
conformance class covers activation transactions, owned tasks, streams,
durable checkpoints, failure recovery, and system observations
(`source/01-scope.md:34`). Each named scenario family is therefore applicable
to a full-Runtime claim; this record excludes none of them. This applicability
determination is an inference from the Runtime class scope and the explicit
ORNA-CONF-006 families, not an additional normative rule.

This file records applicability only. It makes no full-Runtime or product
conformance claim. The scenario IDs below are representative indexed examples
for each required family, not an exhaustive test inventory.

## Applicable reference scenarios

The listed scenario IDs and titles are from `tests/scenarios.json`. The frozen
index labels each as an implementation scenario not executed by an Orna
engine.

| ORNA-CONF-006 family | Applicable indexed scenarios |
| --- | --- |
| Assertion | `ASSERT-CHECKPOINT-091` — assertion failure leaves a coupled stream checkpoint unchanged; `ASSERT-TABLE-CANDIDATE-091` — table assertions see the complete candidate relation; `ASSERT-CROSSTABLE-091` — only genuine cross-table invariants use module scope. |
| Automatic failure | `FAILURE-PROPAGATE-091` — failure automatically skips remaining expression work; `FAILURE-ROLLBACK-091` — unhandled failure rolls back activation writes. |
| Recovery | `RECOVERY-FAILURE-091` — recovery handles one reaching failure; `RECOVERY-REFAIL-091` — a recovery handler may re-emit or replace failure; `RECOVERY-SUCCESS-091` — recovery stage is skipped on success. |
| Conversion | `CONVERT-MULTISOURCE-091` — one target accepts several explicit source conversions; `CONVERT-NOCHAIN-091` — conversion lookup never invents an implicit chain. |
| Transaction | `TXN-001` — activation rolls back nested writes; `TXN-002` — successful activation commits together; `TXN-003` — external effect is not undone. |
| Cancellation | `CANCELLATION-091` — cancellation is not broad failure recovery. |
| Cross-table validation | `ASSERT-CROSSTABLE-091` — only genuine cross-table invariants use module scope. |

## Evidence status

The frozen `tests/requirement-evidence.json` entry for `ORNA-CONF-006`
records `implementation_result: not executed`, the linked test status as
`planned`, and `full_implementation_coverage_claimed: false`. The scenario
index entries cited above are authored test inputs/oracles, not engine
execution results. Accordingly, all seven applicability families remain
required before any full-Runtime claim; this record does not mark them passed.

No Rust or Orna source was changed, so this documentation-only increment ran
no cargo tests. It adds no Orna fixture and no inline Orna source.
