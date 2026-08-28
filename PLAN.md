# OrnaDB remaining implementation plan

## Status and scope

This plan covers the remaining roadmap surfaces after the accepted `orna.std/9`
V9 standard snapshot, bounded `std.data.Rows`/retained table route, V9
structural UI constructors, Qt v1 runtime/provider boundary, bounded populated
Inspector projections, CLIENT resource/transport slices, and headless ordinary
CLIENT Inspector/render contracts.

It tracks implementation hardening, external release evidence, and contract
work for the remaining production-runtime extensions, richer Inspector/model
semantics, Studio workflows, and reflective gateways. The accepted Qt v1,
bounded populated Inspector, V8 Rows/table, and V9 constructor slices are
implementation baselines. The canonical specification still marks broader
extensions as `CURRENT PROPOSAL` or `OPEN`; this plan must not turn them into
implementation contracts. Installed package/clean-host proof, general
Rows/object-value semantics, models, reflective gateways, and remote transport
remain proposal-, contract-, or environment-gated.

- The external `../TODO.md` checklist records the completed CLIENT, TTY,
  Inspector-core/populated-row, source, security, presenter, state, resource,
  action, identity, transport, JSON, Qt/runtime-conformance, LSP, V8/V9, and
  verified editor/tooling/demo slices, plus editor-runtime (Neovim/Vim),
  installed-package/host-proof, and gateway blockers. It is intentionally
  outside `work/` and outside Git.

- Work ADRs 0068-0079 define the closed CLIENT expression, state, JSON,
  resource, transport, action, and test-only runtime contracts. The accepted
  resource chain is explicit: [0071](docs/decisions/0071-client-resource-lifecycle.md)
  owns lifecycle identity, [0077](docs/decisions/0077-client-server-resource-language.md)
  owns source constructors and `AWAIT`, [0078](docs/decisions/0078-client-server-resource-transport.md)
  owns transport and scheduling, and [0079](docs/decisions/0079-client-action-values.md)
  owns executable `std.action.call` values. Work ADRs [0080](docs/decisions/0080-client-inspector.md)
  and [0081](docs/decisions/0081-standard-inspector-render-contract.md) define
  the headless ordinary CLIENT Inspector v1 and the generic
  `std.inspect.render@1` contract; 0081 supersedes only 0080's product-specific
  helper naming.
- `docs/decisions/0062-std-ui-value-type.md` accepts the transient `std.ui.UI`
  value; ADRs 0082-0085 now accept the bounded Linux x86_64 Qt v1
  runtime/provider, loader, and package boundary. Additional toolkits,
  platforms, models, and installed/clean-host proof remain separate gates.
- `docs/decisions/0064-sys-inspect-core.md`, work ADRs 0080-0081, and ADR 0086
  define the current Inspector boundary. Bounded resource/UI/presentation/
  runtime rows are accepted and implemented; richer lifecycle, model, full UI
  tree, and projection semantics remain deferred.
- `docs/decisions/0075-std-json-value.md` and
  `docs/decisions/0079-client-action-values.md` define the append-only V5 JSON
  and V6 action snapshots. Work ADR 0077 owns the accepted CLIENT-to-SERVER
  language surface, with 0078 as its transport/scheduling successor and 0079
  as its executable action successor; these ADRs do not accept the proposal-
  level model, gateway, or runtime surfaces.

The canonical research plan remains useful as historical evidence, but this
plan is the current implementation projection. Accepted ADRs and
`../spec/docs/02-status-decisions.md` remain authoritative.

## Evidence status (2026-08-28)


- **Current validated focused evidence:** the `work/` checkout records the
  focused package, protocol, compiler, LSP, no-`STATE` dogfood, resource-span,
  buffered-preflight, standard-upgrade, runtime-handle, editor-tooling,
  demo-runner, Compose, Qt runtime/ABI/package, and Studio smoke evidence
  below. Workspace reports remain historical and were not rerun. Installed Qt
  package selection, clean-host, Neovim/Vim host sessions, manual Zed/VSIX
  runtime parity, live REF, same-major PostgreSQL, and remaining contract
  gates remain pending or unclaimed; resource-terminal provenance is closed by
  ADR 0078/commit `767991d`.

## Current validated focused evidence (`work/` checkout)

- `cargo test -p orna-protocol sealed_event_batch_uses_event_tag_and_result_credit_lifecycle` — 1 test returned success.
- `cargo test -p orna-server --lib sealed_` — 4 tests returned success.
- `cargo test -p orna-lsp --test lsp_e2e serves_accepted_client_semantic_tokens_with_utf16_and_nested_ranges` — 1 test returned success after correcting the expected UTF-16 token range.
- `cargo test -p orna-server --test standard_database checks_and_evaluates_accepted_client_local_assignment_fixture_offline` — 1 test returned success after correcting the fixture test's durable function-identity conversion.
- `cargo test --locked -p orna-server --test standard_database checks_and_evaluates_accepted_client_control_flow_fixture_offline -- --exact` — 1 test returned success; the accepted bounded CLIENT control-flow fixture checks, prepares, authorises, and evaluates to INTEGER 5.
- `just demo-check` — all 10 runnable accepted source-check/offline demos returned success; the compose-only Inspector entry was skipped by design.
- `cargo test -p orna-compiler accepts_scalar_resource_assignment_await_with_exact_spans_and_call_provenance` — 1 test returned success.
- `cargo test -p orna-compiler discovers_stream_resource_target_with_resolved_element_type` — 1 test returned success.
- `cargo test -p orna-server buffered_sealed_cancel_prevents_acceptance_for_all_preflight_outcomes` — 1 test returned success.
- `cargo test -p orna-standard prepares_the_v3_to_v4_standard_upgrade_from_an_empty_v3_active_revision` — 1 test returned success.
- `cargo test -p orna-standard prepares_the_v5_to_v6_standard_upgrade_from_an_empty_v5_active_revision` — 1 test returned success.
- `cargo test -p orna-client same_revision_terminal_replacement_persists_when_new_evaluation_fails` — 1 test returned success.
- `cargo test -p orna-client same_revision_terminal_replacement_persists_when_later_expression_fails` — 1 test returned success.
- `cargo check -p orna-client -p orna-compiler -p orna-lsp -p orna-postgres -p orna-protocol -p orna-server -p orna-standard -p orna-syntax` — returned success with existing dead-code warnings.
- `cargo fmt --all -- --check` — returned success.
- `cargo test -p orna-core --lib observer_` — 3 Inspector observer-context and
  immutable-clone tests returned success.
- `cargo test -p orna-client --lib` — 326 tests returned success after the
  current-invocation executor hook and Qt fallback forwarding were added.
- `cargo test -p orna-server --lib inspector_` — 13 Inspector binding,
  recursion, and carrier tests returned success.
- `cargo test -p orna-postgres --lib inspect` — 13 Inspector codec and storage
  tests returned success with the existing two warnings.
- `cargo test -p orna-postgres --features test-hooks --test bootstrap -- --ignored
  --test-threads=1` — 20 migration/bootstrap tests returned success, including
  the v45 Inspector observer-context schema.
- With the Compose PostgreSQL service, the focused installed Inspector evaluator
  proof returned success, and the server half of the subsequent matrix passed
  the installed Inspector, recursion, stale-session, source-apply, and
  `standard_database` suites. The combined matrix was not claimed as a full
  pass because its PostgreSQL phase used stale migration-registry expectations
  before the focused bootstrap correction.
- Historical report (not current validation): a prior `cargo test --workspace --all-targets` run recorded 2545 tests, 0 failed, and 235 ignored across 40 suites; it was not rerun for this checkout, so no current workspace result is claimed.
- Historical report (not current validation): a later proof report recorded 2578 workspace tests and 14 LSP protocol tests plus 31 accepted tree-sitter cases; these checks were not rerun for this checkout and are not claimed as current passes.
- Historical report (not current validation): a prior `just editor-tooling-check` run recorded 14 LSP protocol tests and the 31-case tree-sitter corpus; it was not rerun for this checkout.
- `python3 scripts/check-editor-tooling.py` passed on 2026-08-27 over 49 `.orna` files: static editor metadata, generated grammar, tree-sitter accepted corpus, Zed ORDER BY captures, TextMate/Vim/Emacs parity, VS Code checks, Helix configuration, and LSP protocol checks passed. `nvim` is absent; `/usr/bin/vi` is Vim 9.2 Tiny without `-syntax` or `-channel`, so the Vim result is filetype-only smoke rather than host-session proof. Manual Zed/VSIX runtime parity is not claimed.
- `cargo test -p orna-lsp --test lsp_e2e serves_final_field_name_through_accepted_rename_transition` — 1 final-field rename navigation test returned success; this focused proof is current. Zed ORDER BY highlight parity remains pending.
- Historical report (not current validation): `cargo test -p orna-client --lib`
  recorded 278 client unit tests returned success. The current source record in
  `../TODO.md` (Concurrent follow-up wave, 2026-08-25) reports a later
  283-test client-library run; this plan does not claim a new execution here.
  Installed evaluator and USER-state database proofs remain Compose-gated.
- `cargo test -p orna-client --lib client_user_state_load_rejects_wrong_instance_key_atomically -- --nocapture` — 1 test returned success; a mismatched USER instance key is rejected before admission and prior state remains unchanged.
- `cargo test -p orna-postgres --lib resource_lineage_validation_ -- --nocapture` — 4 tests returned success; zero request, parent, and call-site lineage identities are rejected before later request validation, and valid identities remain accepted without mutating the request. Live producer/audit database proof remains environment-gated.
- `cargo test -p orna-compiler --lib named_standard_resource_result_uses_catalogue_value_identity -- --exact --nocapture` — 1 test returned success; standard value identities remain durable in the CLIENT resource plan.
- Historical/unverified focused report (not current validation):
  `cargo test -p orna-postgres --lib sealed_output_ -- --nocapture` covered
  empty or mismatched sink offers failing closed before presenter execution,
  while exact and generic ByteStream and Document offers pass. The earlier
  suite count is intentionally omitted because current evidence records
  disagree.
- Historical/unverified focused report (not current validation):
  `cargo test -p orna-postgres --lib kernel::server_execution::tests
  -- --nocapture` covered sealed presentation after threading client sink
  offers through it. The earlier suite count is intentionally omitted because
  current evidence records disagree.
- `cargo test -p orna-compiler resolver::tests::rejects_client_resource_table_descriptor_with_deferred_row_diagnostic -- --exact --nocapture` and `rejects_client_stream_resource_record_descriptor_with_deferred_row_diagnostic` — 2 parser-accepted resolver regressions returned success; deferred inline TABLE/RECORD resource descriptors emit the exact `TypeMismatch` diagnostic and no checked bundle.
- `cargo test -p orna-compiler resolver::tests::discovers_stream_resource_target_with_resolved_enum_element_type -- --exact --nocapture` and `rejects_opaque_stream_resource_elements_without_a_checked_bundle` — 2 stream element boundary proofs returned success; durable enum elements resolve through CLIENT `STREAM` resources and opaque elements fail closed.
- `cargo test -p orna-client --lib tests::client_expression_call_depth_is_bounded_by_artifact_limit -- --nocapture` — 1 test returned success; the evaluator rejects expression call depth above the artifact limit.
- `cargo test -p orna-protocol --lib resource_values_reject_truncated_value_length_without_mutating_connection -- --nocapture` — 1 test returned success; malformed value-length decoding preserves connection state and unrelated stream credit.
- `cargo test -p orna-compiler --lib accepts_external_client_parameters_and_capabilities -- --nocapture` — 1 test returned success; external CLIENT capability declarations remain retained through resolution. Installed enforcement remains Compose-gated.
- `cargo test -p orna-standard --lib v3_to_v4_upgrade_rejects_non_v3_parents_before_child_work -- --nocapture` — 1 test returned success; the wrong-parent upgrade fails before child work.
- `cargo test -p orna-compiler --lib accepted_client_action_preparation_preserves_durable_operation_identity_and_arguments -- --nocapture` — 1 test returned success; prepared CLIENT action identity and normalized arguments remain durable.
- `cargo test -p orna-artifact --lib procedural_plan_round_trips_scalar_resource_await_in_assignment -- --nocapture` — 1 test returned success; the accepted ADR 0077 artifact matrix preserves ordered locals, scalar resource construction, AWAIT assignment/return, exact target revision/call-site, and canonical parameter identities.
- `cargo test -p orna-compiler accepts_a_checked_server_function_with_a_relational_plan` — 1 test returned success; the checked SERVER plan preserves DESC ordering, `NullOrder::Unspecified`, stable field identities, and exact checked query-reference order.
- `cargo test -p orna-syntax --test accepted_subset` — 3 tests returned success; the accepted subset rejects a PL/SQL-style CLIENT `EXCEPTION` tail while preserving source text and the stable `ORNA0001` diagnostic.
- With the test-hooks feature and the local PostgreSQL test service, `cargo test -p orna-postgres --test recovery authenticated_resource_denies_recovered_security_definer_before_execution -- --ignored --exact --nocapture` returned success; direct resource SECURITY DEFINER dispatch is denied before execution and finalizes its resource/invocation audit state. The read-only fixture does not independently observe a target write; follow-up proof remains tracked separately.

- With the test-hooks feature and the local PostgreSQL test service, `cargo test -p orna-server --features test-hooks --test standard_database authenticated_direct_resource_post_reservation_failure_is_compensated_once -- --ignored --exact` returned success; the direct resource failure path compensates one reserved request. This does not replace the remaining hostile-search-path recovery proof.
- `cargo test -p orna-protocol frame::tests::` — 84 protocol frame tests returned success after rejecting stale `InvokeEvents` during cancellation, preserving the accepted operational/audit failure exception, and rejecting late `CALL_ACCEPTED` after pre-accept cancellation. `cargo check -p orna-protocol` also passed with existing warnings.
- `cargo test -p orna-client --lib client_user_state_` (8 passed), `cargo test -p orna-server --lib user_state` (12 passed), and `cargo check -p orna-client -p orna-server` (passed with existing warnings) — principal isolation checks passed.
- `cargo test -p orna-server --test standard_database checks_accepted_client_state_fixture_plan_metadata_offline` and `cargo test -p orna-lsp --test lsp_e2e serves_canonical_accepted_dogfood_fixtures_without_diagnostics` — accepted CLIENT STATE fixture and LSP corpus checks passed.
- `python3 -m py_compile scripts/check-editor-tooling.py` and `python3 scripts/check-editor-tooling.py` — static editor tooling and fail-closed Zed metadata validation passed on 2026-08-27 over 49 `.orna` files. `nvim` is absent; `/usr/bin/vi` is Vim 9.2 Tiny without `-syntax` or `-channel`, so the Vim result is filetype-only smoke. Manual Zed/VSIX runtime parity and GUI/runtime launch remain unclaimed.
- `cargo test -p orna-protocol resource_connection_rejects -- --nocapture` — 12 resource connection tests returned success; duplicate/skip sequence and terminal mismatch proofs preserve connection and credit state on rejection.

The initial LSP expectation mismatch, CLIENT fixture compile mismatch, and
workspace private-import compile failure were development corrections, not gate
failures. The current `just kernel-test` matrix covers the Compose/Docker
installed evaluator and Inspector-recursion paths. Clean-host, Neovim/Vim host
sessions, manual Zed/VSIX runtime parity, installed Qt package selection, live
REF, same-major PostgreSQL, and remaining contract results remain pending or
unclaimed.
- **Historical implementation inventory (not current status):** the 2026-08-25
  nested work list included cancellation, raw-preflight, SecurityAdmin,
  sealed-invocation, compiler, and Compose/clean-host items. The current
  accepted slices and focused/Compose evidence are recorded above; remaining
  gates are installed Qt package selection, clean-host, Neovim/Vim host
  sessions, manual Zed/VSIX runtime parity, live REF, same-major PostgreSQL,
  resource-terminal provenance, and proposal-only contract work.
- **Historical evidence (not current validation):** the post-audit reports and
  nested commit IDs below are retained for provenance only. The outer checkout
  has only `master` ref `ff31b4e`; nested work commit IDs are not verifiable
  as current evidence.



## Current accepted implementation slices (2026-08-27)

The following source and contract boundaries are present in the current
`work/` checkout. Focused results are current evidence; historical reports
remain labelled, and no environment-gated proof is inferred.

- Accepted implementation commits include `75c5766` (ADR 0080 Inspector
  observer-bound epochs, INEP v2, migration 45, and trusted current-dispatch
  binding), `f55df8d` (retained V8 table executable), `fd21c1c` (Inspector
  selected-sink carrier classification), `c68bb0a` (variable-cell Rows
  validation), `b260ac5`/`50a0cfc` (generic CLIENT named-argument
  order/coverage), `113012c` (client Clippy cleanup), `34e39f4` (safe
  Postgres Clippy cleanup), `57c0bf9` (ADR 0090 local authority), `767991d`
  (resource provenance contract), `8d3e4cd` (operator runbook), `33aa485`
  (V9 constructor showcase), `6e69500` (reachable text-input showcase root),
  and `3d23e5b` (reachable button showcase root).
  Evidence updates are recorded by `2f8509f`, `07ecc28`, `880ca17`, and
  `ca831a5`.
- `python3 scripts/check-editor-tooling.py` passed over 49 `.orna` files;
  `python3 scripts/run-demos.py` passed with the unsupported generic scalar
  fixture excluded, including the `ui-constructor-showcase` source-check demo
  with reachable text, button, panel, row, column, text_input, tabs, and
  window roots. The earlier accepted baseline `just kernel-test` result is
  `artifact://22571`; the current Inspector observer evidence is listed above.
  Runtime/ABI/package/Studio smokes passed as recorded session evidence.
  Installed package selection and clean-host proof remain pending.
- Closed issue boundaries include `ornadb-el1.3.2.1.1` (Rows/table),
  `ornadb-el1.5.13.7`/`ornadb-el1.5.13.9` (retained V8 presenter and
  presenter-only resource admission), `ornadb-el1.5.14.2`/`ornadb-el1.5.14.7`
  (V9 constructors and generic named arguments), `ornadb-el1.3.4.3`
  (ADR 0090 local authority), `ornadb-el1.4.4` (SERVER dogfood),
  `ornadb-br0.2` (developer tools), and `ornadb-br0.4.5` with children
  `.1` and `.2` (reachable V9 constructor showcase). Broad security parent
  `ornadb-el1.3.4` remains open. Resource terminal provenance
  `ornadb-el1.2.1.43` is closed by ADR 0078/commit `767991d`; the accepted
  commit-receipt authority and producer evidence boundary are reconciled.
- Generic no-FROM parameter projection (`ornadb-el1.3.14`) and scalar demo
  (`ornadb-br0.4.4`) remain blocked by ADR 0055: `NoInputParameterSelect` is
  accepted only for fixed standard `std.invoke.echo` in `orna.std/2`; the
  canonical spec/standard-source contract does not accept a generic parameter
  projection or generic scalar executable.


## Language and standard-library dogfooding

The answer to the current concern is yes: future work must make Orna
programs more useful as source, not only as examples around Rust-backed kernel
contracts. The current `stdlib/std/*.orna` files are intentionally small
because the accepted slices still expose sealed host boundaries. That is a
valid boundary for implementation, but it is not a sufficient language
dogfooding story.

This work has three separate goals:

1. **Grammar integration.** Keep `spec/spec/orna.ebnf` as the canonical
   proposal source, then connect accepted productions to parser fixtures,
   syntax-tree tests, diagnostics, editor grammars, and runnable source
   examples. Do not generate or freeze the parser from the current proposal
   until the grammar status changes.
2. **Standard-library source parity.** For every accepted standard function or
   value contract, prefer a real `.orna` declaration and body where the
   language can express it. Keep Rust code only at explicit kernel, codec,
   security, storage, or host-runtime boundaries. Each remaining sealed
   boundary must have a documented reason and a focused proof.
3. **Runnable dogfood applications.** Add small source programs that use the
   accepted language and standard library through the same parse, check,
   install, and invoke path as user programs. Resource model/consumer
   semantics beyond the accepted scalar and stream forms, graphical runtimes,
   populated Inspector resource/UI projections, and reflective gateways stay
   out of these fixtures until their contracts become executable.

- use the existing accepted CLIENT expression and state syntax as the first
  source base, then extend it only when the corresponding contract is accepted;
- add a grammar conformance gate for the accepted subset;
- add one complete accepted server-function dogfood program, then identify the
  next presenter or launcher path that can move into `.orna` source;
- make each source program run through an installed proof;
- then repeat the same pattern for accepted resource and later Inspector or
  gateway contracts as each contract becomes executable.

This is a product requirement, not optional documentation polish. A fully
featured language needs source users can read, modify, check, and run.

## Historical post-audit record (not current validation)

The entries in this section preserve earlier implementation and evidence
reports. They do not establish current validation for the outer checkout.

The 2026-08-22 locked-scope audit found no missing accepted executable slice.
It did find and close evidence and tooling defects without broadening the
contract:

- retained V5 JSON and V6 action source, catalogue, digest, and codec parity
  now have focused goldens in `crates/orna-standard/src/lib.rs`;
- canonical scalar spellings now have parser, LSP, and tree-sitter coverage;
- the Inspector ORV5 carrier rejects duplicate and descending record-field
  identities;
- the lifecycle verifier accepts only the exact normal or SIGQUIT-escalated
  stop sequence and derives the escalation report flag from that sequence;
- the Zed integration now registers the real grammar and the existing
  `orna-lsp` stdio binary; this static registration does not establish accepted
  ORDER BY highlight parity, which remains pending;
- direct resource revision pinning was audited against the accepted locks and
  needs no source change.
- bare `has_privilege` checks now fail closed for inactive principals and reject
  object-scoped INSPECT requests; the installed security-admin proof covers both
  enforcement boundaries.
- applied migration SQL remains append-only; commit `a4d6ff0` restored the
  exact version-23, version-29, and version-30 bytes after the live bootstrap
  gate detected a checksum mismatch. The focused bootstrap proof and the
  historical local/default command `cargo test --workspace --all-targets`
  passed; `just kernel-test` is Docker Compose/ignored PostgreSQL evidence,
  not current clean Debian-host proof.
- the installed source-apply path now records one fixed-principal protected
  `SourceApply` event, and audit recovery checks both the principal and the
  historical source/catalogue pair;
- retained revision-pair listing now validates all decoded ancestry in memory,
  including parent parity, cycles, identity uniqueness, and the single active
  marker, without per-entry database round trips;
- commit `6e61996` now enforces retained V1 source identity before checking the
  source: the canonical source-unit ID, `std/types.orna` logical path, and
  ordinal are required. Local evidence is
  `crates/orna-compiler/src/resolver.rs::tests::rejects_v1_source_unit_identity_mutations`.
- commit `c58869d` now orders checked resource and action arguments by their
  canonical `CheckedParameterId`, independent of source named-argument order.
  Local evidence is
  `crates/orna-compiler/src/resolver.rs::tests::sorts_resource_and_action_arguments_by_checked_parameter_id`.
- commit `ba20fda` now proves private headless runtime teardown retires owned
  node, action, model, and request handles, cancels owned requests once, and
  rejects stale work. Local evidence is
  `crates/orna-client/src/lib.rs::runtime_conformance::destroying_a_surface_retires_all_owned_handles_and_suppresses_stale_work`
  (the focused runtime-conformance run passed 39 tests); this is not production
  runtime-ABI evidence.
- Historical nested report associated with `1aeef47` records accepted SERVER
  dogfood fixture checks and offline preparation for four functions and a
  candidate revision pair. The named evidence path is
  `crates/orna-server/tests/standard_database.rs::checks_and_prepares_server_function_dogfood_fixture_offline`;
  no current rerun is claimed here;
  PostgreSQL installation and invocation remain Compose-gated.
- commit `8af410c` adds a focused source-apply error mapping regression; the
  existing `CatalogueInvariant` marker maps to `RecoveryMismatch`. It does not
  claim to execute post-apply recovery or prove candidate-hash reproduction.
- commit `64d9603` covers both retained legacy Inspector public error-code
  aliases; the accepted Inspector boundary still has no populated projection
  or graphical-runtime claim.
- The prior report listed CLIENT lifecycle evidence including bounded stream queue rollback and
  dequeue capacity release (`crates/orna-client/src/lib.rs::client_stream_queue_overflow_preserves_existing_batches`
  and `crates/orna-client/src/lib.rs::client_stream_queue_dequeue_releases_capacity`),
  sequential terminal action re-trigger identity and repeated-pending rejection
  (`action_trigger_after_terminal_completion_allocates_fresh_request_identity`
  and `action_trigger_rejects_repeated_pending_server_request_without_mutating_generation`),
  nested replacement retention and exact child identity
  (`replacing_resource_key_retains_nested_request_when_abandon_fails`,
  `nested_action_pending_cancel_retains_replacements_and_exact_child`,
  `nested_action_malformed_child_pending_cancel_retains_exact_identity`, and
  `nested_executor_rejects_mismatched_completion_identity`), and generation
  exhaustion preflight (`resource_invalidation_preflights_generation_before_releasing_request`).
  Commit `538769d` adds the corresponding client and installed raw/broker
  transport ownership hardening. The prior report listed focused client/server
  suites, workspace compilation/tests, source-apply boundary tests, and editor
  tooling as passing;
  detached transport restoration remains conditional on source reusability.
  Stream-action execution, Compose proofs, clean-host proof, and proposal-level
  runtime/Inspector/gateway surfaces remain deferred.

The prior report described focused proof in the repository. The resource,
transport, action, and Inspector slices have local parser/compiler/codec/
carrier checks plus installed proof paths; the installed paths that exercise
PostgreSQL are marked `#[ignore]` because they require the Compose PostgreSQL
development service. No local Compose result is claimed. The source-apply
audit, rollback, tamper, and retained-listing integration tests are likewise
present but Compose-gated. The prior report described compilation, migration registry checks, codec checks,
and focused in-memory validators only.
The commits above add local compiler, private-fixture, offline SERVER-dogfood,
and CLIENT lifecycle evidence only; they do not upgrade any Compose or
host-gated proof, or broaden the accepted boundary. No proposal-level
implementation should start without the contract gate below.
The fresh network-disabled Debian 12 host proof and the same-major PostgreSQL
predecessor transition remain separate blockers. No proposal-level
implementation should start without the contract gate below.


### Historical 2026-08-25 implementation inventory (not current status)

The current nested source contains the following proof work. Focused results
recorded above are current; workspace reports remain historical and were not
rerun, while the static editor-tooling gate passed on 2026-08-25. `nvim` is
absent; `/usr/bin/vi` is Vim 9.2 Tiny without `-syntax` or `-channel`, so
its filetype-only smoke is not host-session proof. Manual Zed/VSIX runtime
proof remains pending or unclaimed.
Installed evaluator/Inspector-recursion, Compose/Docker, clean-host, live REF,
same-major PostgreSQL, and contract gates remain pending or unclaimed. `nvim` is
absent; `/usr/bin/vi` is Vim 9.2 Tiny without `-syntax` or `-channel`, so its
filetype-only smoke is not host-session proof. Manual Zed/VSIX runtime parity
remains unclaimed:

- same-revision terminal replacement retention across staged CLIENT evaluator
  errors (same_revision_terminal_replacement_persists_when_new_evaluation_fails
  and same_revision_terminal_replacement_persists_when_later_expression_fails);
  both focused checks are current; broader lifecycle proof remains pending;
- cancellation identity and executor ownership checks (validate_owned_request
  and the mismatched-identity lifecycle fixtures);
- buffered raw CALL_CANCEL handling during preflight
  (buffered_sealed_cancel_prevents_acceptance_for_all_preflight_outcomes and
  buffered_cancel_precedes_first_finish_poll_but_finish_still_runs); the first
  focused check is current and the companion check remains pending;
- resource assignment and AWAIT source spans
  (accepts_procedural_scalar_resource_local_await and
  discovers_stream_resource_target_with_resolved_element_type); both focused
  checks are current; broader compiler proof remains pending;
- the accepted no-`STATE` CLIENT procedural local/assignment/`RETURN`
  source-dogfood fixture at
  `crates/orna-server/tests/fixtures/client_local_assignment_dogfood.orna`
  with the offline check
  `checks_and_evaluates_accepted_client_local_assignment_fixture_offline`;
  its focused offline check is current; workspace validation and installed evaluator/Inspector-recursion and Compose proof remain pending;
- LSP UTF-16 position and semantic-token coverage, including
  `orna-lsp/tests/lsp_e2e.rs::serves_accepted_client_semantic_tokens_with_utf16_and_nested_ranges`;
  its focused check is current; the static editor-tooling gate passed on
  2026-08-25, while Neovim/Vim host-session and manual Zed/VSIX runtime proof
  remain pending or unclaimed;
- focused positive V3-pinned and V5-pinned standard-library upgrade checks:
  `prepares_the_v3_to_v4_standard_upgrade_from_an_empty_v3_active_revision`
  and `prepares_the_v5_to_v6_standard_upgrade_from_an_empty_v5_active_revision`;
  both focused checks are current; installed/Compose proof remains pending;
- SecurityAdmin privilege filtering for inactive principals and object-scoped
  INSPECT requests (`security_admin_live.rs::prove_has_privilege_filters`);
- raw sealed-invocation queued terminal/cancellation handling, including
  `dispatch_completion_has_claimed_terminal` and the buffered cancellation
  checks in `crates/orna-server/src/raw_socket.rs`;
- trusted audit-recovery search-path establishment before loading audit rows;
  implementation is present, but the direct recovery proof remains
  Compose-gated and pending;
- ordinary CLIENT call/REF-field compiler fixtures plus accepted resource,
  action, and LSP/editor fixtures; resource and final-field rename focused proofs are current;
  static editor-tooling validation passed on 2026-08-25, while ordinary-call and
  installed evaluator/Inspector-recursion, Neovim/Vim host-session, and manual
  Zed/VSIX runtime proof remain pending or unclaimed.

- The CLIENT expression-depth, truncated `RESOURCE_VALUES`, external-capability,
  V3-to-V4 parent, action-preparation, and REF-field checks listed above are
  current focused evidence; their installed, Compose, or live boundaries remain
  separately pending.

- the Zed grammar pin at `editors/zed/extension.toml:[grammars.orna].rev`
  (`f5c9007ee2ba8dcd00784e806a9d9b32be6efe08`) is implementation-present and
  enforced by `scripts/check-editor-tooling.py`; the current pin passed the
  static editor gate on 2026-08-25, and a temporary alternate 40-hex revision
  was rejected. The check validates metadata identity, not remote grammar bytes
  or Zed GUI runtime parity.
- resource-request reservation recovery and hostile-search-path audit recovery
  remain Compose-gated proof-pending tests in
  `crates/orna-postgres/tests/recovery.rs`:
  `recovery_rejects_resource_audit_without_its_durable_request_reservation`
  and
  `recovers_closed_security_audit_history_under_hostile_search_path_and_rejects_tamper`.

- Resource cancellation provenance is closed under Bead `ornadb-el1.2.1.43` by
  ADR 0078/commit `767991d`; direct/shared terminal precedence under Bead
  `ornadb-el1.2.1.44` is also closed. The accepted adapter review found no
  concrete precedence mismatch and no wire-contract change.
- ADR 0080 closed the Inspector epoch-width authority blocker tracked by Bead
  `ornadb-el1.2.1.33`: ORNA-INSPECT/1 treats its u64 `epoch_id` as complete v1
  wire authority; full-width epoch identity is deferred to a future carrier
  version and ADR.
- Direct mutation `SECURITY DEFINER` remains a recorded contract/fixture blocker
  under Bead `ornadb-el1.2.1.28.1`; the recovered read-only guard proof does not
  establish mutation side effects.

Run the remaining lifecycle, raw-preflight, compiler, Compose/Docker, clean-host,
Neovim/Vim, live REF, same-major PostgreSQL, and accepted contract checks
before extending the current validated result.

### Historical 2026-08-24 report (not current validation)

A prior nested worktree report recorded workspace/package test counts, cargo
check, and cargo fmt results, plus bounded ORNA-RESOURCE request identity,
target-revision ordering, sealed InvokeEvents terminal precedence,
executor-owned resource completion validation, StreamValues cancellation and
abandon ordering, failed-abandon retention, nested ownership, zero action
identities, redacted source-apply errors, and four source-apply integration
tests. Those results were not rerun and cannot be verified from the outer
checkout.

Direct PostgreSQL recovery and installed source-apply audit remain Compose-gated;
manual VSIX parity was not a tracked package gate or editor-runtime result.
Production graphical runtimes, populated Inspector projections, and gateways
remain pending behind their accepted contract boundaries.

## Historical 2026-08-22 next contract checkpoint (superseded by ADRs 0082-0090)

The previously recorded 2026-08-19 checkpoint is superseded by work ADRs
0076-0081. The current boundary is:

- the test-only headless runtime conformance fixture is accepted and
  implemented; ADRs 0082/0085 now accept the bounded Qt v1 production
  runtime/provider/package boundary, while broader ABI/provider extensions
  remain proposals;
- CLIENT resource language, transport, and scheduling are accepted and
  implemented under [0077](docs/decisions/0077-client-server-resource-language.md)
  and [0078](docs/decisions/0078-client-server-resource-transport.md). Focused
  parser/compiler/codec checks are local; installed resource/transport proofs
  in `crates/orna-server/tests/standard_database.rs` are Compose-gated
  `#[ignore]` tests, so no local Compose result is claimed.
- Executable actions are limited to `std.action.call` under
  [0079](docs/decisions/0079-client-action-values.md); focused plan/trigger
  checks are present, while the installed SERVER-action proof is also
  Compose-gated. `std.action.sequence` and `std.action.parallel` remain
  reserved until a later scheduler contract.
- The ordinary CLIENT Inspector v1 and generic render contract are accepted
  and implemented under [0080](docs/decisions/0080-client-inspector.md) and
  [0081](docs/decisions/0081-standard-inspector-render-contract.md). ADR 0086
  now accepts bounded populated resource/UI/presentation/runtime rows; focused
  carrier/epoch/lineage checks and the current Compose matrix cover that
  bounded scope. Richer model/full-tree projections remain outside it.
- reflective gateways remain on the sealed `sys.invoke` boundary until
  Endpoint, Exposure, Service, authentication, conversion, redaction, and
  protocol lifecycle contracts are accepted.

The next implementation plan is therefore conditional, not an instruction to
start code:

1. Accept one new contract and update the canonical status, work ADR index,
   traceability, and exact source/generated artefact list.
2. Add stable identities, exact errors, ownership, security, compatibility,
   cancellation, and shutdown rules before implementation.
3. Add one focused unit slice and one integration or live proof.
4. Implement the smallest vertical path, then run the package gate and the
   installed proof before extending the surface.

The candidate order is:

1. broader production-runtime ABI/provider extensions beyond the accepted Qt
   v1 boundary, after the canonical ABI resolves their ownership, re-entry,
   thread, event, and shutdown rules;
2. richer Inspector resource/UI model/full-tree projections, after a contract
   resolves epoch ownership, lifecycle capture, privilege, and redaction;
3. reflective gateway contracts and adapters, after the external
   authentication, exposure versioning, conversion, and protocol rules are
   executable.

The Qt v1 runtime and bounded Inspector/Rows/V9 slices are accepted baselines.
Maintain the Debian host evidence blocker and do not implement a second
runtime, richer model/full-tree projections, or a JSON-RPC/MCP gateway from
proposal text alone.

## Non-negotiable contract gate

No implementation phase may start for a proposal-level surface until a new
accepted work ADR and the corresponding canonical status update define all of
these points:

- stable names, identities, and allocation rules;
- exact input, output, and error forms;
- ownership and lifetime rules;
- security, principal, and redaction rules;
- version and compatibility rules;
- failure, cancellation, backpressure, and shutdown behaviour;
- focused tests and one integration or live proof;
- source files, generated files, migrations, and artefacts that must change.

A draft ABI header, narrative example, or traceability row is research evidence,
not an implementation authority.

## Dependency graph

```text
Accepted headless runtime conformance
 |
 +--> [contract gate] broader runtime/provider extensions
 |                                      |
 |                                      +--> Studio
 |
 +--> [contract gate] richer Inspector/model projections
                                        |
                                        +--> Studio Inspector explorer
 |
 +--> [contract gate] Exposure, Service, and protocol contracts
                                        |
                                        +--> JSON-RPC gateway
                                        |
                                        +--> MCP gateway
```

The accepted headless runtime, Qt v1 provider, bounded populated Inspector
rows, V8 Rows/table route, and V9 constructors are baselines. They do not imply
acceptance of a second runtime, richer model/full-tree projections, Studio, or
gateways.

## Phase 0: Contract research and acceptance

Phase 0 is a contract gate. Sections 0.1-0.3 research and accept new
contracts. Section 0.4 hardens the already accepted Inspector contracts. No
proposal-level implementation belongs in this phase.

### 0.1 Production runtime ABI and non-TTY runtime contract

The accepted Qt v1 provider/package boundary is implemented under ADRs 0082 and
0085, with current ABI, loader, package, and Studio smoke evidence. This section
now covers hardening and any later provider extension; it must not relabel Qt
v1 as unimplemented. The test-only headless runtime conformance fixture remains
the semantic oracle. Define only the next extension boundary from the proposal in
`../spec/api/runtime-abi.md`, `../spec/api/ui-runtime.md`,
`../spec/docs/15-runtime-architecture.md`, and
`../spec/spec/orna_runtime_abi_v1.h`.

The contract must settle:

- canonical value representation and who allocates and releases each buffer;
- descriptor and offer validation, feature negotiation, and version matching;
- surface and node ownership, borrowed input lifetime, and callback lifetime;
- thread affinity, re-entrancy, event-loop ownership, and serialisation;
- atomic UI batch rules, semantic revision handling, and stale-batch rejection;
- typed event payloads, model request completion, and cancellation;
- shutdown ordering, outstanding request handling, and failure reporting.

**Deliverables:** focused Qt v1 hardening evidence and, only for a later
provider, an accepted ABI extension ADR, conformance fixtures, and a runtime
proof. Qt v1 is already selected and accepted; do not select a second toolkit
or add model/gateway semantics before its contract is accepted.

**Acceptance:** the selected runtime loads through the accepted ABI, rejects
incompatible descriptors, obeys ownership and thread rules, applies atomic
batches, rejects stale revisions, reports typed events, handles cancellation,
and shuts down cleanly. The proof must cover the same lifecycle cases as the
headless fixture.

**Likely files:** `spec/api/runtime-abi.md`,
`spec/spec/orna_runtime_abi_v1.h`, one new work ADR, and the selected runtime
and client modules.

### 0.2 Populated Inspector resource and UI projection contract

ADR 0086 accepts bounded population of the existing resource/UI/presentation/
runtime Inspector rows, and the current source and Compose matrix cover that
slice. This section now covers richer lifecycle/model/full-tree projection
semantics and Studio integration. Do not describe the bounded rows as empty or
unimplemented.

The contract must settle:

- resource, stream, UI surface, node, and runtime identity schemas;
- snapshot epoch ownership, capture timing, and freeze/resume behaviour;
- projection revision, invalidation, and stale-read handling;
- privilege, principal, source, argument, value, and audit redaction rules;
- bounded carrier sizes, ordering, truncation, and error forms;
- observer suppression and recursion rules for resource and UI inspection;
- the relationship between server epochs and client runtime epochs.

**Deliverables:** hardening for the accepted ADR 0086 carriers plus, only for
richer semantics, an accepted projection ADR/schema, focused tests, and an
installed Inspector proof with redacted resource/UI data. The installed proof
is the current Compose integration evidence; no clean-host or package proof is
inferred.

**Acceptance:** an ordinary CLIENT Inspector can request one permitted
resource or UI projection, receive an immutable checked carrier, reject stale
or over-limit data, suppress observer recursion, and redact values outside the
privilege ladder. The proof must not require a graphical UI sink.

**Likely files:** `spec/docs/30-inspector.md`,
`spec/docs/31-self-inspection.md`, `spec/api/inspect.md`,
`crates/orna-core/src/inspect.rs`, `crates/orna-artifact`,
`crates/orna-client`, `crates/orna-server`, and focused live tests.

### 0.3 Exposure, Service, and gateway contract

Define the explicit reflective gateway model from
`../spec/docs/19-reflective-gateways.md`,
`../spec/api/protocol-gateways.md`, and
`../spec/docs/27-wire-protocol.md`.

The contract must settle:

- `Endpoint`, `Exposure`, version, `AuthPolicy`, and `ResultPolicy` identities;
- `Service` ownership, launch, lifecycle, and principal/delegation rules;
- external authentication and the rule that the request cannot select a
  principal;
- schema generation and lossless JSON-to-canonical-value conversion;
- version pinning, disabled exposure behaviour, error mapping, streaming, and
  cancellation;
- the exact construction of a canonical sealed `sys.invoke` request.

**Deliverables:** accepted value/API and wire ADRs, standard source and
catalogue identities, bounded protocol frames, conversion tests, and a live
proof for one disabled and one enabled exposure.

**Acceptance:** an external request authenticates outside its body, resolves one
explicit versioned exposure, converts only accepted values, invokes sealed
`sys.invoke`, redacts failures, and cannot inject a principal or bypass grants.
No implicit JSON-RPC or MCP publication is allowed.

**Likely files:** `spec/docs/19-reflective-gateways.md`,
`spec/api/protocol-gateways.md`, `spec/docs/27-wire-protocol.md`, one or more
standard source units, `crates/orna-protocol`, `crates/orna-core`,
`crates/orna-client`, `crates/orna-server`, and `crates/orna-postgres`.

### 0.4 Inspector contract

Work ADRs 0080 and 0081 accept the headless ordinary CLIENT Inspector v1 and
its generic standard render contract `std.inspect.render@1`, described by
`../spec/docs/30-inspector.md`, `../spec/docs/31-self-inspection.md`, and
`../spec/api/inspect.md`. This stream now covers implementation hardening and
integration, not acceptance of a proposal-level Inspector contract.

Implementation hardening and integration must preserve:

- the accepted ordinary CLIENT Inspector signature and return value, with
  rendering identified by `std.inspect.render@1` rather than an application
  function name;
- snapshot epoch, freeze, resume, and observer-context semantics;
- recursion and observer suppression for self-inspection;
- immutable projection-carrier identity and revision evidence;
- privilege and redaction rules for arguments, values, source, and audit data;
- the relationship between server snapshot epochs and client runtime epochs.

**Deliverables:** focused hardening changes, checked CLIENT function,
versioned snapshot/projection schema, and the Compose-gated installed
self-inspection proof path.

**Acceptance:** the headless Inspector root continues to inspect another
invocation without executing it, inspect itself without an observer loop,
freeze and resume an epoch, and reject projections outside its privilege
ladder. The bounded populated resource/UI/presentation/runtime rows accepted
by ADR 0086 remain in scope; richer model/full-tree projections and reflective
gateways remain separately gated.

**Likely files:** `crates/orna-core/src/inspect.rs`, `crates/orna-client`,
`crates/orna-compiler`, `crates/orna-artifact`,
`crates/orna-server/src/inspect.rs`, and focused live tests.


## Phase 1: Implement the next accepted contract

Phase 1 starts only after one Phase 0 contract is accepted and its exact
source, generated artefact, and proof list is recorded.

1. Add stable identities, exact errors, ownership, security, compatibility,
   cancellation, and shutdown rules.
2. Add one focused unit slice and one integration or live proof.
3. Implement the smallest vertical path through the existing accepted
   boundary.
4. Keep the tree buildable and preserve all accepted baselines.

Keep each commit to one to three files. Run focused tests after every step,
then run the workspace and installed gates at the phase checkpoint.

## Phase 2: Maintain the headless ordinary CLIENT Inspector baseline

ADRs 0080 and 0081 accept the headless ordinary CLIENT Inspector v1 and
generic standard render contract; the current source contains the implementation
`std.inspect.render@1`. Focused local checks are implementation evidence; installed
self-inspection/evaluator proofs remain Compose-gated, and production-runtime
proof remains outside these ADRs and separately proposal-gated.

The accepted headless baseline and bounded populated-row slice are
implementation-present. Further work in this phase is conditional on the
richer projection/model contract:

1. preserve the checked Inspector signature and stable projection identities;
2. preserve snapshot reads without executing the observed target;
3. preserve recursion suppression, privilege checks, redaction, epoch
   freeze/resume, and the current Compose-gated installed self-inspection path;
4. add only richer resource/UI model rows or full-tree semantics after the
   corresponding contract is accepted.

The current source contains the accepted headless and bounded populated-row
scope. The Compose installed proof is recorded as passed, while installed Qt
package selection, clean-host and native-host proof remain separately gated;
neither is proof of richer model/full-tree projections or reflective gateways.

## Phase 3: Implement the full Studio workflow and UI-runtime extensions

This phase is blocked until the remaining Studio model/launch/exposure
contracts and installed/clean-host evidence are accepted. Qt v1 and bounded
Inspector projections are already implementation baselines. Studio remains an
ordinary CLIENT application, not a new core language construct.

After those contracts are accepted, implement vertical slices in this order:

1. catalogue tree and function/type search;
2. SQL/source editor with offline diagnostics;
3. result grid backed by typed presenters;
4. source apply, semantic diff, and revision browser;
5. security/DBA page through sealed administration functions;
6. runtime and presenter explorer through Inspector projections;
7. source reload and hot-revision flow with explicit state/session identity.

Each Studio slice requires a focused CLIENT proof and one installed end-to-end
proof. Do not add a second toolkit until the first runtime is stable.


## Phase 4: Implement the reflective gateways

This phase is blocked until the Exposure, Service, authentication, conversion,
redaction, and wire lifecycle contracts are accepted.

After acceptance:

1. Register the accepted `std.protocol` and `std.service` value contracts.
2. Add explicit exposure listing, version resolution, schema generation, and
   disabled-exposure rejection.
3. Add one JSON-RPC adapter that authenticates externally and calls sealed
   `sys.invoke`.
4. Add one MCP adapter using the same canonical request boundary and separate
   protocol framing/error tests.
5. Add gateway configuration and inspection through the Studio/Inspector path.

The JSON-RPC and MCP adapters must share conversion and authorisation code but
must keep protocol framing and error mapping separate. Neither adapter may
accept a principal, `run_as`, or arbitrary SQL from an external request.

## Phase 5: Add a second runtime and complete M10-M12 proofs

This phase is blocked until Phase 3 has a proven production runtime and Studio
path, and Phase 4 has accepted gateway contracts.

After those prerequisites:

- implement the second runtime family against the same accepted ABI;
- run the full runtime conformance suite for both runtimes;
- prove automatic selection and explicit override without changing the public
  invocation contract;
- prove Studio source/apply/revision, security, Inspector, presenter, and
  gateway workflows end to end;
- update the traceability matrix only from direct test or artefact evidence.

## Commit and verification policy

For every implementation increment:

- use one to three files per commit;
- use a Conventional Commit subject;
- keep implementation, tests, migrations, and generated identities in coherent
  commits;
- run the focused test before committing;
- run `cargo test --workspace --all-targets` and `just kernel-test` at each
  phase checkpoint;
- run the ABI C syntax check and runtime conformance suite when ABI files change;
- run the clean Debian 12 amd64, network-disabled packaging proof required by
  work ADR 0019 on the required host, not on Fedora or a substitute container;
- do not reformat unrelated files or fabricate a same-major PostgreSQL
  predecessor edge.


## Current blockers

These are contract and external-evidence blockers, not missing implementation
effort:

- `spec/api/runtime-abi.md:1-44` and `spec/api/ui-runtime.md:1-47` retain
  `CURRENT PROPOSAL` status for broader ABI/provider extensions; ADRs 0082 and
  0085 close the bounded Qt v1 ownership, lifetime, threading, loader, and
  package boundary.
- `spec/docs/30-inspector.md` and `spec/docs/31-self-inspection.md` lock the
  headless carrier and recursion boundaries. ADR 0086 accepts bounded populated
  resource/UI/presentation/runtime rows; `spec/api/inspect.md` remains
  `CURRENT PROPOSAL` only for richer projection/model semantics.
- `spec/api/protocol-gateways.md:1-3` and the wire protocol remain
  `CURRENT PROPOSAL`; Endpoint, Exposure, Service, authentication, conversion,
  redaction, and lifecycle details are not executable.
- Work ADR 0019 requires a clean Debian 12 amd64, network-disabled host proof,
  which is not available on the current Fedora host.
- The current `just kernel-test` matrix passed the Compose PostgreSQL and
  installed evaluator/Inspector-recursion paths. Standalone package selection,
  clean-host proof, and any unrun environment-specific source-apply/resource/
  action/security-admin/invocation result remain separately gated.
- Offline/static editor tooling is accepted, but editor-runtime proof remains
  blocked: `nvim` is absent, `/usr/bin/vi` is Vim 9.2 Tiny without `-syntax`
  or `-channel` and only provides filetype-only smoke. Manual Zed/VSIX runtime
  parity is not claimed.
- Full Studio remains pending behind richer model/launch/projection contracts
  and installed/clean-host proof; the accepted Qt v1 runtime, bounded Inspector
  rows, V8 table route, and V9 constructors are not a full Studio path.
- The checked-in `debian-clean-machine.sh` scenario currently runs its proof
  inside Docker (`crates/orna-system-tests/scenarios/debian-clean-machine.sh:22-27`).
  That is useful isolation evidence but does not satisfy the accepted host
  proof. Obtain a fresh Debian 12 amd64 host or VM runner with networking
  disabled, run the same package and lifecycle matrix without Docker, archive
  machine/package/manifest/process/trace evidence, and update the CI proof path
  before restoring the checked item in `../TODO.md`.
- The same-major PostgreSQL upgrade remains intentionally deferred until a
  real successor release declares a predecessor edge.

Until these blockers change status, maintain the accepted Qt v1, bounded
Inspector, V8 Rows/table, V9 constructor, and developer-tool baselines while
improving evidence and preparing the next contract. Do not implement a second
runtime, richer Inspector/model projections, full Studio workflows, or
JSON-RPC/MCP gateways from proposal text alone.
