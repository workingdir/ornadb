# ORNA-CONF-004 extension-preservation evidence

Release epic: `ornadb-1787968123319-16-24513f57` (GitHub #89), increment 5  
Base: `origin/main` at `f74580136303c9e2398eee5ab4a81daf6276a0d3`  
Reference: `/home/pbox/dev/ornadb/reference/Orna-1.0.0`, release `1.0.0`  
Publication SHA-256: `d12cf5d86b9337ccbe45f257bcb8c25bc769e0505500bdc68e21a1b67d728d7d`

## Clause and scope

The frozen `source/01-scope.md` defines **ORNA-CONF-004** as: “Extensions MUST NOT change the meaning of valid source, repository trees or protocol messages in the claimed profile.” This record covers the protocol-message extension boundary only. It is evidence for the protocol profile's behavior, not a complete implementation-conformance result across source, repositories, or every protocol scenario.

The requested “full-runtime scenario” wording belongs to **ORNA-CONF-006** in the same frozen source; that clause lists assertion, automatic-failure, recovery, conversion, transaction, cancellation, and cross-table validation scenarios. This increment does not claim or record ORNA-CONF-006 evidence.

## Executed protocol evidence

The protocol-v1 tests exercise extension behavior at decode, re-encode, and request-identity boundaries:

| Test | Observed assertion |
| --- | --- |
| `request_fingerprint_covers_extensions` | Adding an extension changes the canonical request fingerprint, so an ignored extension is still bound to request identity. |
| `result_body_extraction_preserves_ignorable_result_extensions` | An ignorable result extension survives extraction. |
| `rejects_malformed_envelopes_and_extensions` | Malformed envelopes and unsupported extension forms fail closed. |
| `protected_values_are_rejected_in_optional_live_extensions` | Protected values remain rejected at the optional-extension boundary. |

Captured command:

```text
cargo test --locked --offline -p orna-protocol-v1
```

Captured output: `/tmp/ornadb-release-conf004-epic89/orna-protocol-v1.log`  
Result: 28 unit tests passed; 10 integration tests passed; 0 doc tests; exit code 0.

These tests use protocol values directly and do not load Orna source. No `.orna` fixture was added or needed. No full-runtime, source-extension, repository-tree, or whole-product conformance claim is made by this record.

## Evidence boundary

This is selected protocol-profile evidence for extension preservation and identity. It does not establish that all valid source, repository trees, or protocol messages are extension-invariant, nor does it change the frozen requirement registry's implementation result or assert a conformance class. The release epic remains open for its other increments.
