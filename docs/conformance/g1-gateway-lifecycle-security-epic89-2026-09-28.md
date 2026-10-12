# G1 gateway lifecycle/security proof — documented deferral

Date: 2026-09-28. Beads task: `ornadb-1787968157244-283-f9013ddd`; verified GitHub issue #383. Test base: `8b4264caf38fd83210419ca24402135d308b1708`.

## Reference check and scope

The frozen authority is `/home/pbox/dev/ornadb/reference/Orna-1.0.0`. The source-defined adjacent contracts are:

- `source/26-security.md:5-9`: ORNA-TRUST-001 (local OS-user trust model), ORNA-TRUST-002 (loopback default and trusted remote boundary), ORNA-TRUST-003 (no public anonymous mutation or arbitrary remote REPL execution).
- `source/28-serving.md:7-23`: ORNA-SERVE-001, ORNA-SERVE-004, ORNA-SERVE-005 and ORNA-SERVE-006 (serving surfaces, loopback default, trust boundary, and explicit/status-reported non-loopback exposure).
- `source/30-protocol.md:9`: ORNA-PROTO-001 (complete validation before operation admission).
- `source/30-protocol.md:23`: ORNA-PROTO-002 (session resume preserves reservations/outcomes and runtime-generation changes invalidate old handles).
- `source/30-protocol.md:13-21,98-121` also specify session creation/resume/delete, authentication and Origin checks, error statuses, request admission, cancellation, disconnect lease, and cleanup joining in prose; those paragraphs have no ORNA-* identifier of their own.

Captured reference search:

```text
rg -n -i 'G1|gateway|JSON-RPC|MCP gateway' /home/pbox/dev/ornadb/reference/Orna-1.0.0 --glob '*.md' --glob '*.json' --glob '*.orna' --glob '*.ebnf' --glob '*.toml' --glob '!evidence/**'
CAPTURED_REFERENCE_SEARCH_EXIT_CODE=1
```

There were no matches in the canonical text, APIs, profiles, grammar, examples, or test artifacts searched. The corresponding repository path search for `gateway|jsonrpc|json-rpc|mcp` under `crates` also returned no matches (exit 1). Therefore the reference and this source tree do not define or implement a G1/JSON-RPC/MCP gateway contract. In particular, there is no normative gateway exposure registry, body-authority mapping, gateway target-denial rule, or gateway result-conversion rule to test without inventing behavior. Those G1-specific acceptance points are deferred pending a normative contract and adapter. This does not defer the adjacent live-protocol requirements listed above.

## Executed bounded live-host evidence

Full Cargo output was captured on the execution host in `/var/tmp/orna-gateway-live-host-383.log` and `/var/tmp/orna-gateway-local-socket-383.log`.

1. `cargo test --locked --offline -p orna-live-v1 --test live_host -- --nocapture` — **exit 0**. Captured result: `test result: ok. 111 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.55s`.
2. `cargo test --locked --offline -p orna-live-v1 --test live_host accepted_tcp_socket_routes_a_session_request_end_to_end -- --exact --nocapture` — **exit 0**. Captured result: `test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 110 filtered out; finished in 0.00s`.

The 111-test run includes passing cases `accepted_tcp_socket_routes_a_session_request_end_to_end`, `malformed_create_json_is_rejected_before_session_admission`, `live_http_routes_are_exact_origin_checked_and_rotate_scoped_tokens`, `delete_requires_current_unexpired_bearer_and_original_origin`, `frames_are_bounded_binary_canonical_and_cancellable`, `websocket_connection_driver_emits_exact_canonical_result_envelope`, `delete_cancels_durable_session_work_before_returning_success`, `failed_child_join_after_durable_cancellation_never_reports_delete_success`, `http_contract_has_stable_status_headers_and_redacted_errors`, and `websocket_input_malformed_application_message_closes_and_retires_attachment`.

These are actual local live-host/protocol tests, including one loopback TCP socket integration test. They are not gateway tests and do not prove G1-specific body authority, exposure selection, conversion, or target policy. No Orna source was added in this evidence-only change; no new `.orna` fixture was required. No reference register was changed.
