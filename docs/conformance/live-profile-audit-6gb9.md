# Live protocol and session profile audit (ornadb-6gb9)

Audit base: clean `origin/main` at `ea5d9d4a6978d352951b9422f7b53974205d55a3`. The frozen profile is the reference at `/home/pbox/dev/ornadb/reference/Orna-1.0.0`; the new test reads it from `ORNA_REFERENCE_DIR` or the sibling `reference/Orna-1.0.0` tree. No production source was changed. This slice closes five profile-linked test-evidence gaps in the new `live_profile_conformance_6gb9.rs` integration test.

## Verified normative anchors

- `ORNA-PROTO-001` — `source/30-protocol.md:9`: decode and validate the full canonical message, fields, and limits before admission; reject duplicate keys/noncanonical forms.
- `ORNA-PROTO-002` — `source/30-protocol.md:23`: session resume preserves reservations and terminal outcomes for the live lease; a new runtime generation invalidates old operational handles.
- `ORNA-PROTO-003` — `source/30-protocol.md:92`: client applies ordered patch operations to a temporary tree and publishes atomically only when base revision and every operation are valid.
- `ORNA-PROTO-004` — `source/30-protocol.md:113`: durable reservation and terminal transactional claim are distinct; successful writes and claim share one transaction; reservation alone never permits replay.
- `ORNA-WIRE-001` through `ORNA-WIRE-006` — `source/14-pages.md:46-56`: initial full snapshot, stable watch ID/revisions and atomic delta application, exact patch vocabulary/identity rules, bounded queues, and full-snapshot reconnect.
- `ORNA-WIRE-009` — `source/14-pages.md:98`: exact envelope fields, deterministic CBOR, and profile encodings are repository/server conformance obligations.

`profiles/live-protocol.md:3-4` identifies `source/30-protocol.md#protocol`, `profiles/live-messages.json`, and `profiles/session.schema.json` as the normative source and two machine-readable artifacts; it defines no independent ORNA-* identifiers.

## Evidence-graded profile obligations

| Obligation | Implementation evidence | Test evidence at audited tip | Grade / result |
|---|---|---|---|
| Envelope v1 has exactly keys 0–4, canonical definite encodings, rejects duplicate map keys and unknown envelope fields; registry contains all 13 code/name/direction/request/watch/body shapes. | `crates/orna-protocol-v1/src/lib.rs:282-318`, `:325-362`, `:415-678`. | Existing exhaustive `every_registry_message_round_trips_stably` (`crates/orna-protocol-v1/src/lib.rs:2151-2162`) and malformed/extension tests (`:2531-2565`); new profile-linked registry and client-control tests. | A for in-process codec; direct artifact linkage now tested. |
| Required and optional body fields, shared context structures, extension ranges and value sizes agree with `live-messages.json`. | Message decoding enforces required numeric keys, type decoders and unknown mandatory extension rejection in `crates/orna-protocol-v1/src/lib.rs:553-678`; context exact-key checks are at `:691-747`. | Existing every-message codec round trip; new registry test checks exact code/name/direction/rules/field-key sets, envelope fields, extension ranges and UInt/bytes definitions. | A for tested schema/codec correspondence; independent external client correspondence remains separate. |
| Create/resume JSON is exactly the two profile strings; duplicate, extra, malformed and wrongly typed members must not reach session admission. | `crates/orna-live-v1/src/lib.rs:6029-6047`, `:6089-6100`, and `:7511-7550` reject duplicate keys, non-string values and any member count other than two before authority calls. | Existing malformed create/resume tests (`crates/orna-live-v1/tests/live_host.rs:6915-6978`); new test validates valid input against `create_request` and checks duplicate/unknown/wrongly typed rejection with zero authority and credential-issuer calls. | A. |
| Create/resume responses provide every `session_response` field, UUID/token/path formats, positive lease/limits and profile minimums. | `session_response` serializes all listed values in `crates/orna-live-v1/src/lib.rs:7674-7711`; `TransportLimits::validate` enforces positive bounds, a <=300,000 ms lease, and retention >= lease at `:4905-4921`. | Existing route/retention evidence (`crates/orna-live-v1/tests/live_host.rs:6177-6184`, `:6778-6808`); new create test validates actual response against `session.schema.json`, including nested limits, path/session equality and retention >= lease. | A for schema projection and current response; host deployment values remain host-specific. |
| Resume credential is canonical unpadded Base64url, path-bound, rotates token, and retains session/database/path identity. | `decode_token`/`decode_base64url` at `crates/orna-live-v1/src/lib.rs:7738-7770`; resume verifies exact retained session credential at `:6105-6138`. | Existing reconnect tests; new resume test validates request/response against the schema, rejects nonzero unused token bits and a mismatched session path, then verifies successful rotation and stable identities. | A for adapter seam; independent reconnect/interoperability remains unproven by this suite. |

## Top-five profile-linked proof gaps closed here

1. No test previously loaded `live-messages.json` and checked the complete message registry against the implementation; the new registry test does.
2. No test connected profile envelope fields, unknown-key policy, extension ranges, and core UInt/byte-size definitions to the tested live package; the new test records these profile assertions and round-trips representative control messages.
3. Create-session request tests did not validate an accepted request against `session.schema.json` or jointly prove duplicate/extra/wrong-type rejection before admission; the new tests do both.
4. Session response tests asserted selected values, not the complete schema and minimums; the new create-response test recursively checks actual fields against the checked-in schema.
5. Resume tests did not jointly test profile request/response schemas, token canonical bits, path/session binding and stable identities; the new resume test covers those profile constraints.

The new suite contains five tests and uses profile files as its fixtures. It submits no Orna source, so no `.orna` fixture is applicable. Existing protocol tests exercise every codec message and existing live tests exercise HTTP/reconnect behavior; this audit does not claim a production interoperability certificate.

## Remaining evidence boundary

`source/30-protocol.md:123` says independent client/server execution and malformed-frame fuzzing remain separate server-profile conformance obligations. `source/14-pages.md:106` conditions a production server-profile claim on independent interoperability, loss/reorder/reconnect simulation, malformed-input fuzzing, bounded slow-client tests, and equality with reference vectors. This slice added deterministic profile-linked conformance tests only; it does not supply an independent implementation, a fuzz campaign, or a production deployment certificate. No reference-undefined behavior was added.
