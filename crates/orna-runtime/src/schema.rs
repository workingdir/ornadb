//! The durable runtime schema, its creation and every in-place migration.

use crate::codec::{decode_position, decode_u64, fixed};
use crate::error::RuntimeError;
use crate::observation::stream_partition_required;
use crate::stream_admin::admin_digest;
use crate::{RuntimeIdentity, RuntimeState};
use ed25519_dalek::SigningKey;
use libsql::{Connection, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::Path;
use uuid::Uuid;

pub(crate) const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
-- Runtime durability profile: journal_mode=WAL and synchronous=FULL. In WAL mode
-- FULL asks the local SQLite-compatible VFS to sync the WAL before a successful
-- commit; process and OS crash durability therefore depend on the host filesystem
-- honoring those sync requests. Power-loss durability is not claimed because
-- device-cache and hardware behavior are outside this flush guarantee. These are
-- the configured settings; no latency or power-cut benchmark is claimed.
PRAGMA synchronous = FULL;
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS runtime_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    database_id BLOB NOT NULL CHECK (length(database_id) = 16),
    repository_id BLOB NOT NULL CHECK (length(repository_id) = 16),
    runtime_id BLOB NOT NULL CHECK (length(runtime_id) = 16),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    generation_digest BLOB NOT NULL CHECK (length(generation_digest) = 32),
    compact_receipt_seed BLOB CHECK (compact_receipt_seed IS NULL OR length(compact_receipt_seed) = 32),
    compact_receipt_public_key BLOB CHECK (compact_receipt_public_key IS NULL OR length(compact_receipt_public_key) = 32)
);
CREATE TABLE IF NOT EXISTS runtime_schema_migration (
    migration TEXT PRIMARY KEY CHECK (length(migration) > 0)
);
CREATE TABLE IF NOT EXISTS writer_lease (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
    epoch INTEGER NOT NULL CHECK (epoch > 0),
    lock_owner_id BLOB CHECK (lock_owner_id IS NULL OR length(lock_owner_id) = 16)
);
CREATE TABLE IF NOT EXISTS takeover_recovery_pending (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
    owner_epoch INTEGER NOT NULL CHECK (owner_epoch > 0)
);
CREATE TABLE IF NOT EXISTS pending_mutation (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    mutation_id BLOB NOT NULL UNIQUE CHECK (length(mutation_id) = 16),
    payload BLOB NOT NULL,
    digest BLOB NOT NULL CHECK (length(digest) = 32)
);
-- Every admitted mutation keeps its identity and digest for the life of the
-- database. Publication prunes `pending_mutation`, so exact-replay detection
-- must not depend on it (ORNA-VFS-008).
CREATE TABLE IF NOT EXISTS admitted_mutation (
    mutation_id BLOB PRIMARY KEY CHECK (length(mutation_id) = 16),
    digest BLOB NOT NULL CHECK (length(digest) = 32)
);
INSERT OR IGNORE INTO admitted_mutation (mutation_id, digest)
    SELECT mutation_id, digest FROM pending_mutation;
CREATE TABLE IF NOT EXISTS accepted_content_pin (
    pin_id BLOB PRIMARY KEY CHECK (length(pin_id) = 16),
    repository_id BLOB NOT NULL CHECK (length(repository_id) = 32),
    database_id BLOB NOT NULL CHECK (length(database_id) = 16),
    descriptor_oid_algorithm INTEGER NOT NULL CHECK (descriptor_oid_algorithm IN (1, 2)),
    descriptor_oid BLOB NOT NULL CHECK (
        (descriptor_oid_algorithm = 1 AND length(descriptor_oid) = 20) OR
        (descriptor_oid_algorithm = 2 AND length(descriptor_oid) = 32)
    ),
    content_length INTEGER NOT NULL CHECK (content_length >= 0),
    content_sha256 BLOB NOT NULL CHECK (length(content_sha256) = 32),
    owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
    owner_epoch INTEGER NOT NULL CHECK (owner_epoch > 0),
    checkpoint_generation INTEGER CHECK (
        checkpoint_generation IS NULL OR checkpoint_generation > 0
    ),
    FOREIGN KEY (checkpoint_generation) REFERENCES checkpoint(generation)
);
CREATE TABLE IF NOT EXISTS table_row (
    table_id TEXT NOT NULL CHECK (length(table_id) > 0),
    row_key BLOB NOT NULL,
    row_value BLOB NOT NULL,
    row_digest BLOB NOT NULL CHECK (length(row_digest) = 32),
    PRIMARY KEY (table_id, row_key)
);
-- A row written under a committed table identity is stored under that
-- identity, so a reader that names the table has to resolve it first
-- (bead ornadb-jm0d).
CREATE TABLE IF NOT EXISTS table_identity (
    table_name TEXT NOT NULL CHECK (length(table_name) > 0),
    object_id BLOB NOT NULL CHECK (length(object_id) = 16),
    PRIMARY KEY (table_name, object_id)
);
CREATE TABLE IF NOT EXISTS checkpoint (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    generation INTEGER NOT NULL UNIQUE CHECK (generation >= 0),
    digest BLOB NOT NULL CHECK (length(digest) = 32),
    mutation_sequence INTEGER NOT NULL,
    accepted_pin_manifest_digest BLOB NOT NULL
        DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000'
        CHECK (length(accepted_pin_manifest_digest) = 32)
);
-- Runtime checkpoints are retained after their pending mutation prefix is
-- published. Keep table row versions independently so a local generation
-- remains readable after publication has pruned that prefix.
CREATE TABLE IF NOT EXISTS runtime_table_history (
    mutation_sequence INTEGER NOT NULL CHECK (mutation_sequence >= 0),
    table_id TEXT NOT NULL CHECK (length(table_id) > 0),
    row_key BLOB NOT NULL,
    row_value BLOB,
    row_digest BLOB,
    deleted INTEGER NOT NULL CHECK (deleted IN (0, 1)),
    PRIMARY KEY (mutation_sequence, table_id, row_key),
    CHECK (
        (deleted = 1 AND row_value IS NULL AND row_digest IS NULL)
        OR (deleted = 0 AND row_value IS NOT NULL AND length(row_digest) = 32)
    )
);
CREATE INDEX IF NOT EXISTS runtime_table_history_lookup
    ON runtime_table_history (table_id, row_key, mutation_sequence);
CREATE TABLE IF NOT EXISTS runtime_table_history_metadata (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    floor_generation INTEGER NOT NULL CHECK (floor_generation >= 0),
    floor_digest BLOB NOT NULL CHECK (length(floor_digest) = 32)
);
-- These boundaries make append-only governance observations part of an exact
-- checkpoint view. Audits themselves do not advance the table generation.
CREATE TABLE IF NOT EXISTS runtime_history_governance_boundary (
    generation INTEGER PRIMARY KEY CHECK (generation >= 0),
    admin_audit_sequence INTEGER NOT NULL CHECK (admin_audit_sequence >= 0),
    checkpoint_reset_audit_sequence INTEGER NOT NULL CHECK (checkpoint_reset_audit_sequence >= 0)
);
CREATE TABLE IF NOT EXISTS publication_freeze (
    intent_id BLOB PRIMARY KEY CHECK (length(intent_id) = 16),
    checkpoint_generation INTEGER NOT NULL,
    checkpoint_mutation_sequence INTEGER NOT NULL,
    checkpoint_digest BLOB NOT NULL CHECK (length(checkpoint_digest) = 32),
    compact_watermark BLOB CHECK (compact_watermark IS NULL OR length(compact_watermark) = 32),
    compact_commit_id BLOB CHECK (compact_commit_id IS NULL OR length(compact_commit_id) IN (40, 64)),
    compact_journal_verifier BLOB CHECK (compact_journal_verifier IS NULL OR length(compact_journal_verifier) = 32),
    frozen INTEGER NOT NULL CHECK (frozen = 1),
    CHECK (
        (compact_watermark IS NULL AND compact_commit_id IS NULL AND compact_journal_verifier IS NULL)
        OR (compact_watermark IS NOT NULL AND compact_commit_id IS NOT NULL AND compact_journal_verifier IS NOT NULL)
    )
);
CREATE TABLE IF NOT EXISTS publication_commit (
    intent_id BLOB PRIMARY KEY REFERENCES publication_freeze(intent_id),
    commit_id BLOB NOT NULL CHECK (length(commit_id) IN (40, 64)),
    compact_receipt BLOB
);
CREATE TABLE IF NOT EXISTS publication_metadata (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    compressed_target_bytes INTEGER NOT NULL DEFAULT 16777216
        CHECK (compressed_target_bytes BETWEEN 8388608 AND 33554432),
    published_mutations INTEGER NOT NULL DEFAULT 0 CHECK (published_mutations >= 0),
    published_payload_bytes INTEGER NOT NULL DEFAULT 0 CHECK (published_payload_bytes >= 0),
    last_publication_ms INTEGER
);
INSERT OR IGNORE INTO publication_metadata (singleton) VALUES (1);
CREATE TABLE IF NOT EXISTS request_ledger (
    session_id BLOB NOT NULL CHECK (length(session_id) = 16),
    request_id BLOB NOT NULL CHECK (length(request_id) = 16),
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    state INTEGER NOT NULL CHECK (state IN (1, 2, 3, 4, 5)),
    terminal_outcome BLOB,
    owner_id BLOB,
    owner_epoch INTEGER,
    effect_evidence INTEGER NOT NULL DEFAULT 0 CHECK (effect_evidence IN (0, 1, 2)),
    recovery_disposition INTEGER NOT NULL DEFAULT 0 CHECK (recovery_disposition IN (0, 1, 2)),
    controlled_transaction_proof BLOB,
    controlled_rollback_proof BLOB,
    PRIMARY KEY (session_id, request_id),
    CHECK (
        (state IN (1, 2) AND terminal_outcome IS NULL)
        OR (state IN (3, 4, 5) AND terminal_outcome IS NOT NULL)
    ),
    CHECK (terminal_outcome IS NULL OR length(terminal_outcome) <= 16777216),
    CHECK ((owner_id IS NULL) = (owner_epoch IS NULL)),
    CHECK (owner_id IS NULL OR length(owner_id) = 16),
    CHECK (owner_epoch IS NULL OR owner_epoch > 0)
);
CREATE TABLE IF NOT EXISTS request_admission (
    session_id BLOB NOT NULL CHECK (length(session_id) = 16),
    request_id BLOB NOT NULL CHECK (length(request_id) = 16),
    fingerprint BLOB NOT NULL CHECK (length(fingerprint) = 32),
    capability_digest BLOB NOT NULL CHECK (length(capability_digest) = 32),
    owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
    owner_epoch INTEGER NOT NULL CHECK (owner_epoch > 0),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    PRIMARY KEY (session_id, request_id)
);
CREATE TABLE IF NOT EXISTS session_deletion (
    session_id BLOB PRIMARY KEY CHECK (length(session_id) = 16),
    owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
    owner_epoch INTEGER NOT NULL CHECK (owner_epoch > 0),
    state INTEGER NOT NULL CHECK (state IN (1, 2))
);
CREATE TABLE IF NOT EXISTS stream_checkpoint (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    consumer_principal TEXT NOT NULL CHECK (length(consumer_principal) > 0),
    consumer_root TEXT NOT NULL CHECK (length(consumer_root) > 0),
    consumer_function TEXT NOT NULL CHECK (length(consumer_function) > 0),
    consumer_binding TEXT NOT NULL CHECK (length(consumer_binding) > 0),
    source_format TEXT NOT NULL CHECK (length(source_format) > 0),
    source TEXT NOT NULL CHECK (length(source) > 0),
    partition_format TEXT NOT NULL CHECK (length(partition_format) > 0),
    partition TEXT CHECK (partition IS NULL OR length(partition) > 0),
    position_format TEXT NOT NULL CHECK (length(position_format) > 0),
    version INTEGER NOT NULL CHECK (version >= 0),
    committed_position TEXT,
    next_fence INTEGER NOT NULL CHECK (next_fence >= 0)
);
CREATE TABLE IF NOT EXISTS stream_checkpoint_history (
    key_id TEXT NOT NULL CHECK (length(key_id) > 0),
    version INTEGER NOT NULL CHECK (version > 0),
    committed_position TEXT NOT NULL CHECK (length(committed_position) > 0),
    snapshot BLOB NOT NULL CHECK (length(snapshot) > 0),
    generation_digest BLOB NOT NULL CHECK (length(generation_digest) = 32),
    transition INTEGER NOT NULL CHECK (transition IN (1, 2)),
    PRIMARY KEY (key_id, version)
);
CREATE TABLE IF NOT EXISTS stream_checkpoint_migration_bridge (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    origin_version INTEGER NOT NULL CHECK (origin_version >= 0),
    origin_position TEXT CHECK (origin_position IS NULL OR length(origin_position) > 0),
    source_generation INTEGER NOT NULL CHECK (source_generation >= 0),
    source_generation_digest BLOB NOT NULL CHECK (length(source_generation_digest) = 32)
);
CREATE TABLE IF NOT EXISTS stream_checkpoint_migration_archive (
    key_id TEXT NOT NULL CHECK (length(key_id) > 0),
    version INTEGER NOT NULL CHECK (version > 0),
    committed_position TEXT NOT NULL CHECK (length(committed_position) > 0),
    snapshot BLOB NOT NULL CHECK (length(snapshot) > 0),
    generation_digest BLOB NOT NULL CHECK (length(generation_digest) = 32),
    transition INTEGER NOT NULL CHECK (transition IN (1, 2)),
    PRIMARY KEY (key_id, version)
);
CREATE TABLE IF NOT EXISTS stream_checkpoint_bootstrap (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    selected_commit TEXT NOT NULL CHECK (length(selected_commit) > 0)
);
CREATE TABLE IF NOT EXISTS stream_failure (
    identity_id TEXT PRIMARY KEY CHECK (length(identity_id) > 0),
    key_id TEXT NOT NULL CHECK (length(key_id) > 0),
    consumer_principal TEXT NOT NULL CHECK (length(consumer_principal) > 0),
    consumer_root TEXT NOT NULL CHECK (length(consumer_root) > 0),
    consumer_function TEXT NOT NULL CHECK (length(consumer_function) > 0),
    consumer_binding TEXT NOT NULL CHECK (length(consumer_binding) > 0),
    source_format TEXT NOT NULL CHECK (length(source_format) > 0),
    source TEXT NOT NULL CHECK (length(source) > 0),
    partition_format TEXT NOT NULL CHECK (length(partition_format) > 0),
    partition TEXT CHECK (partition IS NULL OR length(partition) > 0),
    position_format TEXT NOT NULL CHECK (length(position_format) > 0),
    delivery_position TEXT NOT NULL CHECK (length(delivery_position) > 0),
    successor_position TEXT NOT NULL CHECK (length(successor_position) > 0),
    version INTEGER NOT NULL CHECK (version >= 0),
    attempts INTEGER NOT NULL CHECK (attempts >= 0),
    status INTEGER NOT NULL CHECK (status BETWEEN 1 AND 7),
    diagnostic_code INTEGER NOT NULL CHECK (diagnostic_code BETWEEN 1 AND 6),
    diagnostic_class INTEGER NOT NULL CHECK (diagnostic_class BETWEEN 1 AND 3)
);
CREATE TABLE IF NOT EXISTS stream_failure_payload (
    identity_id TEXT PRIMARY KEY CHECK (length(identity_id) > 0),
    payload BLOB,
    payload_reference TEXT,
    payload_digest BLOB,
    retention INTEGER NOT NULL CHECK (retention IN (1, 2)),
    CHECK (payload_digest IS NULL OR length(payload_digest) = 32),
    CHECK (
        (
            retention = 1
            AND payload IS NOT NULL
            AND payload_reference IS NULL
            AND payload_digest IS NULL
        )
        OR (
            retention = 2
            AND payload IS NULL
            AND payload_reference IS NOT NULL
            AND length(payload_reference) > 0
            AND payload_digest IS NOT NULL
        )
    )
);
-- Assertion detail is deliberately isolated from generic provider failures.
-- It retains only typed coordinates, numeric span data, and optional safe
-- witness text; no source excerpt, row value, or arbitrary value encoding is
-- admitted at this boundary. Legacy failures simply have no companion row.
CREATE TABLE IF NOT EXISTS stream_failure_assertion_detail (
    identity_id TEXT PRIMARY KEY REFERENCES stream_failure(identity_id),
    code TEXT NOT NULL CHECK (code = 'ORNA-A091-009'),
    assertion_ref BLOB NOT NULL,
    owner_ref BLOB NOT NULL,
    owner_kind INTEGER NOT NULL CHECK (owner_kind BETWEEN 1 AND 4),
    predicate_ref BLOB NOT NULL,
    source_file_ref BLOB NOT NULL,
    start_byte BLOB NOT NULL,
    end_byte BLOB NOT NULL,
    start_line BLOB NOT NULL,
    start_column BLOB NOT NULL,
    end_line BLOB NOT NULL,
    end_column BLOB NOT NULL,
    safe_witness TEXT
);
CREATE TABLE IF NOT EXISTS stream_failure_payload_legacy (
    identity_id TEXT PRIMARY KEY CHECK (length(identity_id) > 0)
);
CREATE TABLE IF NOT EXISTS stream_provider_failure (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    checkpoint_version INTEGER NOT NULL CHECK (checkpoint_version >= 0),
    committed_position TEXT,
    attempts INTEGER NOT NULL CHECK (attempts > 0),
    diagnostic_code INTEGER NOT NULL CHECK (diagnostic_code BETWEEN 1 AND 5),
    diagnostic_class INTEGER NOT NULL CHECK (diagnostic_class BETWEEN 1 AND 3)
);
CREATE TABLE IF NOT EXISTS stream_lease (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    delivery_position TEXT NOT NULL CHECK (length(delivery_position) > 0),
    successor_position TEXT NOT NULL CHECK (length(successor_position) > 0),
    fence INTEGER NOT NULL CHECK (fence > 0),
    purpose INTEGER NOT NULL CHECK (purpose BETWEEN 1 AND 2)
);
CREATE TABLE IF NOT EXISTS stream_retry_claim (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    identity_id TEXT NOT NULL CHECK (length(identity_id) > 0)
);
-- A provider receipt is keyed by the complete delivery identity, effect
-- ordinal, provider and operation. Request bytes are never retained here;
-- the digest fences a replay from changing the effect for an existing key.
-- Applied receipts are retained because an explicit checkpoint reset may
-- encounter any prior delivery again; pruning would remove its dedupe fence.
CREATE TABLE IF NOT EXISTS stream_provider_effect_receipt (
    effect_identity BLOB PRIMARY KEY CHECK (length(effect_identity) > 0),
    request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
    idempotency_key BLOB NOT NULL CHECK (length(idempotency_key) = 32),
    lease_fence INTEGER NOT NULL CHECK (lease_fence > 0),
    attempts_in_fence INTEGER NOT NULL CHECK (attempts_in_fence > 0),
    total_attempts INTEGER NOT NULL CHECK (total_attempts > 0),
    status INTEGER NOT NULL CHECK (status IN (1, 2, 3, 4)),
    receipt BLOB CHECK (receipt IS NULL OR length(receipt) <= 16777216),
    CHECK ((status = 2 AND receipt IS NOT NULL) OR (status <> 2 AND receipt IS NULL))
);
CREATE TABLE IF NOT EXISTS stream_replay_claim (
    identity_id TEXT PRIMARY KEY CHECK (length(identity_id) > 0),
    version INTEGER NOT NULL CHECK (version >= 0),
    owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
    owner_epoch INTEGER NOT NULL CHECK (owner_epoch > 0)
);
CREATE TABLE IF NOT EXISTS stream_control (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    status INTEGER NOT NULL CHECK (status IN (1, 2))
);
CREATE TABLE IF NOT EXISTS stream_pause_pending (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0)
);
CREATE TABLE IF NOT EXISTS stream_pause_reason (
    key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
    reason TEXT NOT NULL CHECK (length(reason) <= 16777216)
);
CREATE TABLE IF NOT EXISTS stream_checkpoint_reset_audit (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    key_id TEXT NOT NULL CHECK (length(key_id) > 0),
    observed_generation INTEGER NOT NULL DEFAULT -1 CHECK (observed_generation >= -1),
    old_version INTEGER NOT NULL CHECK (old_version >= 0),
    old_position TEXT,
    new_version INTEGER NOT NULL CHECK (new_version >= 0),
    new_position TEXT NOT NULL CHECK (length(new_position) > 0),
    reason TEXT NOT NULL CHECK (length(reason) <= 16777216),
    redacted INTEGER NOT NULL CHECK (redacted IN (0, 1))
);
CREATE TABLE IF NOT EXISTS admin_invocation_audit (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    invocation_id BLOB NOT NULL UNIQUE CHECK (length(invocation_id) = 16),
    function_name TEXT NOT NULL CHECK (length(function_name) > 0),
    safe_arguments TEXT NOT NULL CHECK (length(safe_arguments) <= 16777216),
    owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
    owner_epoch INTEGER NOT NULL CHECK (owner_epoch > 0),
    observed_generation INTEGER CHECK (observed_generation IS NULL OR observed_generation >= 0),
    terminal_outcome TEXT NOT NULL CHECK (length(terminal_outcome) > 0 AND length(terminal_outcome) <= 16777216),
    succeeded INTEGER NOT NULL CHECK (succeeded IN (0, 1)),
    redacted INTEGER NOT NULL CHECK (redacted IN (0, 1))
);
CREATE TABLE IF NOT EXISTS admin_invocation_checkpoint_result (
    invocation_id BLOB PRIMARY KEY REFERENCES admin_invocation_audit(invocation_id),
    version INTEGER NOT NULL CHECK (version >= 0),
    committed_position TEXT NOT NULL CHECK (length(committed_position) > 0)
);
CREATE TABLE IF NOT EXISTS admin_invocation_stale_capture (
    invocation_id BLOB PRIMARY KEY REFERENCES admin_invocation_audit(invocation_id),
    database_id BLOB NOT NULL CHECK (length(database_id) = 16),
    runtime_id BLOB NOT NULL CHECK (length(runtime_id) = 16),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    generation_digest BLOB NOT NULL CHECK (length(generation_digest) = 32)
);

CREATE TABLE IF NOT EXISTS sys_run_observation (
    run_id BLOB PRIMARY KEY CHECK (length(run_id) = 16),
    session_id BLOB NOT NULL CHECK (length(session_id) = 16),
    request_id BLOB NOT NULL CHECK (length(request_id) = 16),
    consumer_identity TEXT NOT NULL CHECK (length(consumer_identity) > 0),
    function_name TEXT NOT NULL CHECK (length(function_name) > 0),
    source_identity TEXT,
    invocation_id BLOB NOT NULL CHECK (length(invocation_id) = 16),
    snapshot BLOB NOT NULL CHECK (length(snapshot) > 0),
    generation_digest BLOB NOT NULL CHECK (length(generation_digest) = 32),
    runtime_id BLOB NOT NULL CHECK (length(runtime_id) = 16),
    runtime_generation INTEGER NOT NULL CHECK (runtime_generation >= 0),
    started_ms INTEGER NOT NULL,
    ended_ms INTEGER,
    observed_ms INTEGER NOT NULL,
    status INTEGER NOT NULL CHECK (status BETWEEN 1 AND 6),
    checkpoint_count INTEGER NOT NULL DEFAULT 0 CHECK (checkpoint_count >= 0),
    diagnostic_code INTEGER,
    diagnostic_class INTEGER,
    CHECK ((diagnostic_code IS NULL) = (diagnostic_class IS NULL))
);
CREATE TABLE IF NOT EXISTS sys_query_observation (
    query_id BLOB PRIMARY KEY CHECK (length(query_id) = 16),
    expression TEXT CHECK (expression IS NULL OR length(expression) BETWEEN 1 AND 16384),
    snapshot BLOB NOT NULL CHECK (length(snapshot) > 0),
    generation_digest BLOB NOT NULL CHECK (length(generation_digest) = 32),
    runtime_id BLOB NOT NULL CHECK (length(runtime_id) = 16),
    runtime_generation INTEGER NOT NULL CHECK (runtime_generation >= 0),
    started_ms INTEGER NOT NULL,
    ended_ms INTEGER,
    observed_ms INTEGER NOT NULL,
    status INTEGER NOT NULL CHECK (status BETWEEN 1 AND 6),
    CHECK ((status IN (1, 2) AND ended_ms IS NULL)
        OR (status IN (3, 4, 5, 6) AND ended_ms IS NOT NULL)),
    CHECK (started_ms <= observed_ms
        AND (ended_ms IS NULL OR (started_ms <= ended_ms AND ended_ms <= observed_ms)))
);
CREATE INDEX IF NOT EXISTS sys_query_observation_by_runtime
    ON sys_query_observation(runtime_id, started_ms, query_id);

CREATE TABLE IF NOT EXISTS sys_invocation_observation (
    invocation_id BLOB PRIMARY KEY CHECK (length(invocation_id) = 16),
    function_object_id BLOB NOT NULL CHECK (length(function_object_id) = 16),
    snapshot BLOB NOT NULL CHECK (length(snapshot) > 0),
    generation_digest BLOB NOT NULL CHECK (length(generation_digest) = 32),
    runtime_id BLOB NOT NULL CHECK (length(runtime_id) = 16),
    runtime_generation INTEGER NOT NULL CHECK (runtime_generation >= 0),
    owner_kind INTEGER NOT NULL CHECK (owner_kind IN (1, 2, 3)),
    parent_invocation_id BLOB REFERENCES sys_invocation_observation(invocation_id),
    run_id BLOB REFERENCES sys_run_observation(run_id),
    owner_session_id BLOB,
    owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
    owner_epoch INTEGER NOT NULL CHECK (owner_epoch > 0),
    started_ms INTEGER NOT NULL,
    ended_ms INTEGER,
    observed_ms INTEGER NOT NULL,
    status INTEGER NOT NULL CHECK (status BETWEEN 1 AND 6),
    result_type_object_id BLOB NOT NULL CHECK (length(result_type_object_id) = 16),
    failure_code TEXT,
    idempotency_key_hash BLOB CHECK (idempotency_key_hash IS NULL OR length(idempotency_key_hash) = 32),
    CHECK ((parent_invocation_id IS NOT NULL AND owner_kind = 1 AND owner_session_id IS NULL)
        OR (parent_invocation_id IS NULL AND owner_kind = 2 AND owner_session_id IS NOT NULL AND length(owner_session_id) = 16)
        OR (parent_invocation_id IS NULL AND owner_kind = 3 AND owner_session_id IS NULL AND run_id IS NOT NULL)),
    CHECK ((status IN (1, 2) AND ended_ms IS NULL AND failure_code IS NULL)
        OR (status = 3 AND ended_ms IS NOT NULL AND failure_code IS NULL)
        OR (status = 4 AND ended_ms IS NOT NULL AND failure_code IS NOT NULL)
        OR (status IN (5, 6) AND ended_ms IS NOT NULL)),
    CHECK (started_ms <= observed_ms AND (ended_ms IS NULL OR (started_ms <= ended_ms AND ended_ms <= observed_ms))),
    FOREIGN KEY (function_object_id, snapshot)
        REFERENCES sys_procedure_materialization(function_object_id, snapshot),
    FOREIGN KEY (result_type_object_id, snapshot)
        REFERENCES runtime_catalogue_type(object_id, snapshot)
);
CREATE TABLE IF NOT EXISTS sys_invocation_argument (
    invocation_id BLOB NOT NULL REFERENCES sys_invocation_observation(invocation_id),
    position INTEGER NOT NULL CHECK (position >= 0),
    name TEXT NOT NULL CHECK (length(name) > 0),
    type_object_id BLOB NOT NULL CHECK (length(type_object_id) = 16),
    value_digest BLOB CHECK (value_digest IS NULL OR length(value_digest) = 32),
    redacted INTEGER NOT NULL CHECK (redacted IN (0, 1)),
    PRIMARY KEY (invocation_id, position),
    UNIQUE (invocation_id, name),
    CHECK (redacted = 0 OR value_digest IS NULL)
);
-- Invocation tails are append-only change notifications. A stable sequence is
-- used instead of wall-clock ordering so equal timestamps and later terminal
-- transitions cannot be skipped by a continuation cursor.
CREATE TABLE IF NOT EXISTS sys_invocation_observation_tail (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    invocation_id BLOB NOT NULL REFERENCES sys_invocation_observation(invocation_id),
    observed_ms INTEGER NOT NULL CHECK (observed_ms >= 0),
    status INTEGER NOT NULL CHECK (status BETWEEN 1 AND 6),
    UNIQUE (invocation_id, observed_ms, status)
);
CREATE INDEX IF NOT EXISTS sys_invocation_observation_tail_by_invocation
    ON sys_invocation_observation_tail(invocation_id, sequence);
-- Backfill once for runtimes that already retained invocation rows before
-- tailing was added. Skip known rows before INSERT: SQLite may advance an
-- AUTOINCREMENT sequence for a duplicate INSERT OR IGNORE, leaving cursor gaps
-- on every restart even though no lifecycle event was appended.
INSERT INTO sys_invocation_observation_tail
    (invocation_id, observed_ms, status)
SELECT observation.invocation_id, observation.observed_ms, observation.status
FROM sys_invocation_observation AS observation
WHERE NOT EXISTS (
    SELECT 1 FROM sys_invocation_observation_tail AS tail
    WHERE tail.invocation_id = observation.invocation_id
      AND tail.observed_ms = observation.observed_ms
      AND tail.status = observation.status
);
CREATE TABLE IF NOT EXISTS runtime_catalogue_identity (
    object_id BLOB PRIMARY KEY CHECK (length(object_id) = 16),
    kind INTEGER NOT NULL CHECK (kind IN (1, 2, 3)),
    created_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS runtime_catalogue_capture (
    snapshot BLOB PRIMARY KEY CHECK (length(snapshot) > 0),
    database_id BLOB NOT NULL CHECK (length(database_id) = 16),
    runtime_id BLOB NOT NULL CHECK (length(runtime_id) = 16),
    generation INTEGER NOT NULL CHECK (generation >= 0),
    generation_digest BLOB NOT NULL CHECK (length(generation_digest) = 32)
);
CREATE TABLE IF NOT EXISTS runtime_catalogue_admission (
    snapshot BLOB PRIMARY KEY CHECK (length(snapshot) > 0),
    FOREIGN KEY (snapshot) REFERENCES runtime_catalogue_capture(snapshot)
);
-- No runtime path binds a new revision pair or reads a binding back; catalogue
-- carry-forward only copies an existing predecessor row to its successor
-- snapshot. The table stays so existing and new state.db files share one
-- schema; dropping it requires a state-schema migration.
CREATE TABLE IF NOT EXISTS runtime_catalogue_revision_binding (
    snapshot BLOB PRIMARY KEY REFERENCES runtime_catalogue_admission(snapshot),
    source_revision_id BLOB NOT NULL CHECK (length(source_revision_id) = 16),
    catalogue_revision_id BLOB NOT NULL CHECK (length(catalogue_revision_id) = 16)
);
CREATE TABLE IF NOT EXISTS runtime_catalogue_revision (
    object_id BLOB NOT NULL REFERENCES runtime_catalogue_identity(object_id),
    snapshot BLOB NOT NULL CHECK (length(snapshot) > 0),
    kind INTEGER NOT NULL CHECK (kind IN (1, 2, 3)),
    qualified_name TEXT NOT NULL CHECK (length(qualified_name) > 0),
    revision_id BLOB NOT NULL CHECK (length(revision_id) = 32),
    semantic_hash BLOB NOT NULL CHECK (length(semantic_hash) = 32),
    PRIMARY KEY (object_id, snapshot),
    UNIQUE (snapshot, qualified_name)
);
CREATE TABLE IF NOT EXISTS runtime_catalogue_module_admission (
    object_id BLOB NOT NULL REFERENCES runtime_catalogue_identity(object_id),
    snapshot BLOB NOT NULL REFERENCES runtime_catalogue_capture(snapshot),
    PRIMARY KEY (object_id, snapshot)
);
CREATE TABLE IF NOT EXISTS runtime_catalogue_type (
    object_id BLOB NOT NULL,
    snapshot BLOB NOT NULL,
    form TEXT NOT NULL CHECK (form IN ('named', 'value', 'reference')),
    target_object_id BLOB,
    PRIMARY KEY (object_id, snapshot),
    FOREIGN KEY (object_id, snapshot)
        REFERENCES runtime_catalogue_revision(object_id, snapshot),
    CHECK ((form = 'reference') = (target_object_id IS NOT NULL)),
    CHECK (target_object_id IS NULL OR length(target_object_id) = 16)
);
CREATE TABLE IF NOT EXISTS runtime_catalogue_function (
    object_id BLOB NOT NULL,
    snapshot BLOB NOT NULL,
    result_type_object_id BLOB NOT NULL CHECK (length(result_type_object_id) = 16),
    PRIMARY KEY (object_id, snapshot),
    FOREIGN KEY (object_id, snapshot)
        REFERENCES runtime_catalogue_revision(object_id, snapshot)
);
CREATE TABLE IF NOT EXISTS runtime_catalogue_parameter (
    object_id BLOB NOT NULL,
    snapshot BLOB NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    name TEXT NOT NULL CHECK (length(name) > 0),
    type_object_id BLOB NOT NULL CHECK (length(type_object_id) = 16),
    PRIMARY KEY (object_id, snapshot, position),
    FOREIGN KEY (object_id, snapshot)
        REFERENCES runtime_catalogue_function(object_id, snapshot)
);
-- A procedure is a runtime materialization of one exact catalogue function.
-- Orna 1.0 exposes the portable declaration as sys.Function; this cache adds
-- no second source identity and is always pinned to that function revision.
CREATE TABLE IF NOT EXISTS sys_procedure_materialization (
    function_object_id BLOB NOT NULL CHECK (length(function_object_id) = 16),
    snapshot BLOB NOT NULL CHECK (length(snapshot) > 0),
    qualified_name TEXT NOT NULL CHECK (length(qualified_name) > 0),
    revision_id BLOB NOT NULL CHECK (length(revision_id) = 32),
    semantic_hash BLOB NOT NULL CHECK (length(semantic_hash) = 32),
    result_type_object_id BLOB NOT NULL CHECK (length(result_type_object_id) = 16),
    materialized_ms INTEGER NOT NULL CHECK (materialized_ms >= 0),
    PRIMARY KEY (function_object_id, snapshot),
    UNIQUE (snapshot, qualified_name),
    FOREIGN KEY (function_object_id, snapshot)
        REFERENCES runtime_catalogue_function(object_id, snapshot),
    FOREIGN KEY (result_type_object_id, snapshot)
        REFERENCES runtime_catalogue_type(object_id, snapshot)
);
CREATE TABLE IF NOT EXISTS sys_procedure_parameter_materialization (
    function_object_id BLOB NOT NULL,
    snapshot BLOB NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    name TEXT NOT NULL CHECK (length(name) > 0),
    type_object_id BLOB NOT NULL CHECK (length(type_object_id) = 16),
    PRIMARY KEY (function_object_id, snapshot, position),
    UNIQUE (function_object_id, snapshot, name),
    FOREIGN KEY (function_object_id, snapshot)
        REFERENCES sys_procedure_materialization(function_object_id, snapshot),
    FOREIGN KEY (type_object_id, snapshot)
        REFERENCES runtime_catalogue_type(object_id, snapshot)
);
CREATE TABLE IF NOT EXISTS sys_stream_observation (
    stream_id BLOB PRIMARY KEY CHECK (length(stream_id) = 16),
    run_id BLOB NOT NULL REFERENCES sys_run_observation(run_id),
    checkpoint_key_id TEXT NOT NULL CHECK (length(checkpoint_key_id) > 0),
    producer TEXT NOT NULL CHECK (length(producer) > 0),
    consumer_name TEXT,
    consumer_identity TEXT NOT NULL CHECK (length(consumer_identity) > 0),
    source_identity TEXT NOT NULL CHECK (length(source_identity) > 0),
    partition TEXT CHECK (partition IS NULL OR length(partition) > 0),
    status INTEGER NOT NULL CHECK (status BETWEEN 1 AND 8),
    items_seen INTEGER NOT NULL DEFAULT 0 CHECK (items_seen >= 0),
    items_committed INTEGER NOT NULL DEFAULT 0 CHECK (items_committed >= 0),
    items_failed INTEGER NOT NULL DEFAULT 0 CHECK (items_failed >= 0),
    checkpoint_version INTEGER,
    last_failure_identity TEXT,
    last_item_ms INTEGER,
    diagnostic_code INTEGER,
    diagnostic_class INTEGER,
    observed_ms INTEGER NOT NULL,
    UNIQUE(run_id, source_identity, partition),
    CHECK ((diagnostic_code IS NULL) = (diagnostic_class IS NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS sys_stream_observation_null_natural_key
    ON sys_stream_observation (run_id, source_identity)
    WHERE partition IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS sys_stream_observation_present_natural_key
    ON sys_stream_observation (run_id, source_identity, partition)
    WHERE partition IS NOT NULL;
"#;

async fn stream_failure_code_requires_upgrade(
    connection: &Connection,
) -> Result<bool, RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'stream_failure'",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RuntimeError::RecoveryInvalid);
    };
    let sql: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
    Ok(!sql.contains("diagnostic_code BETWEEN 1 AND 6"))
}

pub(crate) async fn load_stream_checkpoint_migration_origin(
    connection: &Connection,
    key_id: &str,
) -> Result<(u64, Option<String>), RuntimeError> {
    let mut rows = connection
        .query(
            "SELECT origin_version, origin_position
             FROM stream_checkpoint_migration_bridge
             WHERE key_id = ?1",
            params![key_id],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok((0, None));
    };
    let version = decode_u64(
        row.get::<i64>(0)
            .map_err(|_| RuntimeError::RecoveryInvalid)?,
    )?;
    let position = row
        .get::<Option<String>>(1)
        .map_err(|_| RuntimeError::RecoveryInvalid)?;
    if let Some(position) = &position {
        decode_position(position.clone())?;
    }
    if (version == 0) != position.is_none() {
        return Err(RuntimeError::RecoveryInvalid);
    }
    Ok((version, position))
}

/// Reconstructs result payloads for successful reset receipts written before
/// checkpoint reset results had their own receipt table. The old generic
/// audit and specialized CAS audit share the key/version/position digests, so
/// only a unique exact transition is eligible for backfill.
pub(crate) async fn migrate_admin_checkpoint_reset_receipts(
    connection: &Connection,
) -> Result<(), RuntimeError> {
    let mut receipt_rows = connection
        .query(
            "SELECT invocation_id, safe_arguments,
                    (SELECT version FROM admin_invocation_checkpoint_result AS result
                     WHERE result.invocation_id = audit.invocation_id),
                    (SELECT committed_position FROM admin_invocation_checkpoint_result AS result
                     WHERE result.invocation_id = audit.invocation_id)
             FROM admin_invocation_audit AS audit
             WHERE terminal_outcome = 'checkpoint_reset' AND succeeded = 1
             ORDER BY sequence",
            (),
        )
        .await?;
    let mut receipts = Vec::new();
    while let Some(row) = receipt_rows.next().await? {
        receipts.push((
            row.get::<Vec<u8>>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            row.get::<String>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            row.get::<Option<i64>>(2)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            row.get::<Option<String>>(3)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        ));
    }
    if receipts.is_empty() {
        return Ok(());
    }

    let mut reset_rows = connection
        .query(
            "SELECT key_id, old_version, old_position, new_version, new_position
             FROM stream_checkpoint_reset_audit ORDER BY sequence",
            (),
        )
        .await?;
    let mut resets = Vec::new();
    while let Some(row) = reset_rows.next().await? {
        resets.push((
            row.get::<String>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            row.get::<i64>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            row.get::<Option<String>>(2)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            row.get::<i64>(3)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            row.get::<String>(4)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        ));
    }

    for (invocation_id, safe_arguments, stored_version, stored_position) in receipts {
        let stored_result = match (stored_version, stored_position) {
            (None, None) => None,
            (Some(version), Some(position)) => {
                Some((decode_u64(version)?, decode_position(position)?))
            }
            _ => return Err(RuntimeError::RecoveryInvalid),
        };
        let fields = safe_arguments
            .split(';')
            .filter_map(|field| field.split_once('='))
            .collect::<BTreeMap<_, _>>();
        let Some(key_digest) = fields.get("stream_key_digest") else {
            if stored_result.is_some() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            continue;
        };
        let Some(expected_version) = fields.get("expected_version") else {
            if stored_result.is_some() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            continue;
        };
        let Some(expected_position_digest) = fields.get("expected_position_digest") else {
            if stored_result.is_some() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            continue;
        };
        let Some(target_digest) = fields.get("target_digest") else {
            if stored_result.is_some() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            continue;
        };
        if key_digest.is_empty()
            || expected_version.is_empty()
            || expected_position_digest.is_empty()
            || target_digest.is_empty()
        {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let mut matches = resets.iter().filter(|reset| {
            let (key, old_version, old_position, _, new_position) = *reset;
            let old_position_digest = old_position
                .as_deref()
                .map(|position| admin_digest(position.as_bytes()))
                .unwrap_or_else(|| "none".into());
            admin_digest(key.as_bytes()).as_str() == *key_digest
                && old_version.to_string() == *expected_version
                && old_position_digest == *expected_position_digest
                && admin_digest(new_position.as_bytes()).as_str() == *target_digest
        });
        let Some((_, _, _, version, position)) = matches.next() else {
            if stored_result.is_some() {
                return Err(RuntimeError::RecoveryInvalid);
            }
            continue;
        };
        if matches.next().is_some() {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let reconciled_version = *version;
        let reconciled_position = position.clone();
        let reconciled = (
            decode_u64(reconciled_version)?,
            decode_position(reconciled_position.clone())?,
        );
        match stored_result {
            Some(stored) if stored != reconciled => {
                // The specification does not define repair for contradictory
                // durable receipts. Fail closed so a replay cannot report a
                // checkpoint transition different from the retained CAS audit.
                return Err(RuntimeError::RecoveryInvalid);
            }
            Some(_) => {}
            None => {
                connection
                    .execute(
                        "INSERT INTO admin_invocation_checkpoint_result
                         (invocation_id, version, committed_position) VALUES (?1, ?2, ?3)",
                        params![invocation_id, reconciled_version, reconciled_position],
                    )
                    .await?;
            }
        }
    }
    Ok(())
}

/// Adds an honest temporal baseline when opening a database created before
/// table history existed. Earlier generations cannot be reconstructed from
/// the current table image, so the recorded floor prevents false empty reads.
pub(crate) async fn migrate_runtime_table_history(
    connection: &Connection,
) -> Result<(), RuntimeError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let mut history_metadata = transaction
        .query(
            "SELECT floor_generation, floor_digest
             FROM runtime_table_history_metadata WHERE singleton = 1",
            (),
        )
        .await?;
    if let Some(row) = history_metadata.next().await? {
        let floor_generation = decode_u64(
            row.get::<i64>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        let floor_digest: [u8; 32] = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        let mut runtime_meta = transaction
            .query(
                "SELECT generation FROM runtime_meta WHERE singleton = 1",
                (),
            )
            .await?;
        let current_generation = decode_u64(
            runtime_meta
                .next()
                .await?
                .ok_or(RuntimeError::RecoveryInvalid)?
                .get::<i64>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        if floor_generation > current_generation {
            return Err(RuntimeError::RecoveryInvalid);
        }
        if floor_generation > 0 {
            let mut checkpoint = transaction
                .query(
                    "SELECT digest FROM checkpoint WHERE generation = ?1",
                    params![
                        i64::try_from(floor_generation)
                            .map_err(|_| RuntimeError::RecoveryInvalid)?
                    ],
                )
                .await?;
            let checkpoint_digest: [u8; 32] = fixed(
                checkpoint
                    .next()
                    .await?
                    .ok_or(RuntimeError::RecoveryInvalid)?
                    .get(0)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            if checkpoint_digest != floor_digest {
                return Err(RuntimeError::RecoveryInvalid);
            }
        }
    } else {
        let mut runtime_meta = transaction
            .query(
                "SELECT generation, generation_digest FROM runtime_meta WHERE singleton = 1",
                (),
            )
            .await?;
        let row = runtime_meta
            .next()
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?;
        let floor_generation = decode_u64(
            row.get::<i64>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?;
        let floor_digest: [u8; 32] = fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
        if floor_generation > 0 {
            let mut checkpoint = transaction
                .query(
                    "SELECT digest FROM checkpoint WHERE generation = ?1",
                    params![
                        i64::try_from(floor_generation)
                            .map_err(|_| RuntimeError::RecoveryInvalid)?
                    ],
                )
                .await?;
            let checkpoint_digest: [u8; 32] = fixed(
                checkpoint
                    .next()
                    .await?
                    .ok_or(RuntimeError::RecoveryInvalid)?
                    .get(0)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
            )?;
            if checkpoint_digest != floor_digest {
                return Err(RuntimeError::RecoveryInvalid);
            }
        }
        transaction
            .execute(
                "INSERT INTO runtime_table_history_metadata
                 (singleton, floor_generation, floor_digest) VALUES (1, ?1, ?2)",
                params![
                    i64::try_from(floor_generation).map_err(|_| RuntimeError::RecoveryInvalid)?,
                    floor_digest.to_vec(),
                ],
            )
            .await?;

        let mut current_rows = transaction
            .query(
                "SELECT table_id, row_key, row_value, row_digest FROM table_row",
                (),
            )
            .await?;
        while let Some(row) = current_rows.next().await? {
            let table: String = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let key: Vec<u8> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let value: Vec<u8> = row.get(2).map_err(|_| RuntimeError::RecoveryInvalid)?;
            let digest: [u8; 32] = fixed(row.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?)?;
            if <[u8; 32]>::from(Sha256::digest(&value)) != digest {
                return Err(RuntimeError::RecoveryInvalid);
            }
            transaction
                .execute(
                    "INSERT INTO runtime_table_history
                     (mutation_sequence, table_id, row_key, row_value, row_digest, deleted)
                     VALUES (0, ?1, ?2, ?3, ?4, 0)",
                    params![table, key, value, digest.to_vec()],
                )
                .await?;
        }
    }
    transaction.commit().await.map_err(RuntimeError::from)
}

/// Adds generation cuts for append-only governance records. Older generic
/// audits already carry their observed generation, so legacy cuts include only
/// earlier-generation events; old specialized reset rows have no such evidence
/// and stay visible only through the unpinned audit API.
pub(crate) async fn migrate_historical_governance(
    connection: &Connection,
) -> Result<(), RuntimeError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let mut columns = BTreeMap::new();
    let mut rows = transaction
        .query("PRAGMA table_info(stream_checkpoint_reset_audit)", ())
        .await?;
    while let Some(row) = rows.next().await? {
        columns.insert(
            row.get::<String>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            (),
        );
    }
    if !columns.contains_key("observed_generation") {
        transaction
            .execute(
                "ALTER TABLE stream_checkpoint_reset_audit
                 ADD COLUMN observed_generation INTEGER NOT NULL DEFAULT -1
                 CHECK (observed_generation >= -1)",
                (),
            )
            .await?;
    }
    transaction
        .execute(
            "INSERT OR IGNORE INTO runtime_history_governance_boundary
             (generation, admin_audit_sequence, checkpoint_reset_audit_sequence)
             VALUES (0, 0, 0)",
            (),
        )
        .await?;
    // Prior versions did not persist exact audit sequence cuts beside a
    // checkpoint. For those generations, observations from strictly earlier
    // CWD generations are the conservative reconstructible prefix.
    transaction
        .execute(
            "INSERT OR IGNORE INTO runtime_history_governance_boundary
             (generation, admin_audit_sequence, checkpoint_reset_audit_sequence)
             SELECT checkpoint.generation,
                    COALESCE((SELECT MAX(audit.sequence)
                              FROM admin_invocation_audit AS audit
                              WHERE audit.observed_generation IS NOT NULL
                                AND audit.observed_generation < checkpoint.generation), 0),
                    COALESCE((SELECT MAX(audit.sequence)
                              FROM stream_checkpoint_reset_audit AS audit
                              WHERE audit.observed_generation >= 0
                                AND audit.observed_generation < checkpoint.generation), 0)
             FROM checkpoint WHERE checkpoint.generation > 0",
            (),
        )
        .await?;
    transaction
        .execute(
            "INSERT OR IGNORE INTO runtime_schema_migration (migration)
             VALUES ('historical-governance-boundary-v1')",
            (),
        )
        .await?;
    transaction.commit().await.map_err(RuntimeError::from)
}

pub(crate) async fn runtime_table_history_floor(
    transaction: &Transaction,
) -> Result<(u64, [u8; 32]), RuntimeError> {
    let mut rows = transaction
        .query(
            "SELECT floor_generation, floor_digest
             FROM runtime_table_history_metadata WHERE singleton = 1",
            (),
        )
        .await?;
    let row = rows.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
    Ok((
        decode_u64(
            row.get::<i64>(0)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
        )?,
        fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
    ))
}

pub(crate) async fn migrate_publication_policy_schema(
    connection: &Connection,
) -> Result<(), RuntimeError> {
    let mut columns = connection
        .query("PRAGMA table_info(publication_metadata)", ())
        .await?;
    while let Some(row) = columns.next().await? {
        let name: String = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        if name == "compressed_target_bytes" {
            return Ok(());
        }
    }
    connection
        .execute(
            "ALTER TABLE publication_metadata ADD COLUMN compressed_target_bytes \
             INTEGER NOT NULL DEFAULT 16777216 \
             CHECK (compressed_target_bytes BETWEEN 8388608 AND 33554432)",
            (),
        )
        .await?;
    Ok(())
}

pub(crate) async fn migrate_compact_receipt_schema(
    connection: &Connection,
) -> Result<(), RuntimeError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let mut rows = transaction
        .query("PRAGMA table_info(runtime_meta)", ())
        .await?;
    let mut runtime_meta_columns = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        runtime_meta_columns.insert(
            row.get::<String>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            (),
        );
    }
    for (column, statement) in [
        (
            "compact_receipt_seed",
            "ALTER TABLE runtime_meta ADD COLUMN compact_receipt_seed BLOB",
        ),
        (
            "compact_receipt_public_key",
            "ALTER TABLE runtime_meta ADD COLUMN compact_receipt_public_key BLOB",
        ),
    ] {
        if !runtime_meta_columns.contains_key(column) {
            transaction.execute(statement, ()).await?;
        }
    }
    let mut rows = transaction
        .query("PRAGMA table_info(publication_commit)", ())
        .await?;
    let mut publication_columns = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        publication_columns.insert(
            row.get::<String>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            (),
        );
    }
    if !publication_columns.contains_key("compact_receipt") {
        transaction
            .execute(
                "ALTER TABLE publication_commit ADD COLUMN compact_receipt BLOB",
                (),
            )
            .await?;
    }
    let mut rows = transaction
        .query("PRAGMA table_info(publication_freeze)", ())
        .await?;
    let mut freeze_columns = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        freeze_columns.insert(
            row.get::<String>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?,
            (),
        );
    }
    for (column, statement) in [
        (
            "compact_watermark",
            "ALTER TABLE publication_freeze ADD COLUMN compact_watermark BLOB",
        ),
        (
            "compact_commit_id",
            "ALTER TABLE publication_freeze ADD COLUMN compact_commit_id BLOB",
        ),
        (
            "compact_journal_verifier",
            "ALTER TABLE publication_freeze ADD COLUMN compact_journal_verifier BLOB",
        ),
    ] {
        if !freeze_columns.contains_key(column) {
            transaction.execute(statement, ()).await?;
        }
    }
    transaction
        .execute(
            "INSERT OR IGNORE INTO runtime_schema_migration (migration)
             VALUES ('compact-runtime-receipt-v1')",
            (),
        )
        .await?;
    transaction.commit().await.map_err(RuntimeError::from)
}

pub(crate) async fn initialize_compact_receipt_key(
    connection: &Connection,
) -> Result<(SigningKey, [u8; 32]), RuntimeError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let mut rows = transaction
        .query(
            "SELECT compact_receipt_seed, compact_receipt_public_key
             FROM runtime_meta WHERE singleton = 1",
            (),
        )
        .await?;
    let row = rows.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
    let seed: Option<Vec<u8>> = row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let public_key: Option<Vec<u8>> = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
    let (key, public_key) = match (seed, public_key) {
        (Some(seed), Some(public_key)) => {
            let seed = fixed(seed)?;
            let public_key = fixed(public_key)?;
            let key = SigningKey::from_bytes(&seed);
            if key.verifying_key().to_bytes() != public_key {
                return Err(RuntimeError::CompactReceiptKeyMismatch);
            }
            (key, public_key)
        }
        (None, None) => {
            let seed = compact_receipt_seed();
            let key = SigningKey::from_bytes(&seed);
            let public_key = key.verifying_key().to_bytes();
            let changed = transaction
                .execute(
                    "UPDATE runtime_meta
                     SET compact_receipt_seed = ?1, compact_receipt_public_key = ?2
                     WHERE singleton = 1
                       AND compact_receipt_seed IS NULL
                       AND compact_receipt_public_key IS NULL",
                    params![seed.to_vec(), public_key.to_vec()],
                )
                .await?;
            if changed != 1 {
                return Err(RuntimeError::CompactReceiptKeyMismatch);
            }
            (key, public_key)
        }
        _ => return Err(RuntimeError::CompactReceiptKeyMismatch),
    };
    transaction.commit().await?;
    Ok((key, public_key))
}

fn compact_receipt_seed() -> [u8; 32] {
    let mut seed = [0_u8; 32];
    seed[..16].copy_from_slice(Uuid::new_v4().as_bytes());
    seed[16..].copy_from_slice(Uuid::new_v4().as_bytes());
    seed
}

pub(crate) fn initialize_compact_receipt_public_key(
    path: &Path,
    expected: [u8; 32],
) -> Result<(), RuntimeError> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => {
            file.write_all(&expected)
                .and_then(|()| file.sync_all())
                .map_err(|_| RuntimeError::StorageUnavailable)?;
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let metadata =
                fs::symlink_metadata(path).map_err(|_| RuntimeError::StorageUnavailable)?;
            if !metadata.file_type().is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() != 32
            {
                return Err(RuntimeError::CompactReceiptKeyMismatch);
            }
            let actual = fs::read(path).map_err(|_| RuntimeError::StorageUnavailable)?;
            if actual.as_slice() != expected {
                return Err(RuntimeError::CompactReceiptKeyMismatch);
            }
            Ok(())
        }
        Err(_) => Err(RuntimeError::StorageUnavailable),
    }
}

pub(crate) async fn migrate_accepted_content_pin_checkpoint(
    connection: &Connection,
) -> Result<(), RuntimeError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .await?;
    let mut rows = transaction
        .query("PRAGMA table_info(accepted_content_pin)", ())
        .await?;
    let mut has_descriptor_oid = false;
    let mut has_checkpoint_generation = false;
    while let Some(row) = rows.next().await? {
        let column: String = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        has_descriptor_oid |= column == "descriptor_oid_algorithm";
        has_checkpoint_generation |= column == "checkpoint_generation";
    }
    drop(rows);

    let mut archived_table = transaction
        .query(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'table' AND name = 'accepted_content_pin_legacy_unverified_v1'",
            (),
        )
        .await?;
    let archived_exists = archived_table.next().await?.is_some();
    drop(archived_table);
    if archived_exists {
        let mut archived_count = transaction
            .query(
                "SELECT COUNT(*) FROM accepted_content_pin_legacy_unverified_v1",
                (),
            )
            .await?;
        let count = archived_count
            .next()
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?
            .get::<i64>(0)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        drop(archived_count);
        if count != 0 {
            return Err(RuntimeError::RecoveryInvalid);
        }
    }

    let mut active_count_rows = transaction
        .query("SELECT COUNT(*) FROM accepted_content_pin", ())
        .await?;
    let active_count = active_count_rows
        .next()
        .await?
        .ok_or(RuntimeError::RecoveryInvalid)?
        .get::<i64>(0)
        .map_err(|_| RuntimeError::RecoveryInvalid)?;
    drop(active_count_rows);

    if !has_descriptor_oid {
        if active_count != 0 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        transaction
            .execute("DROP TABLE accepted_content_pin", ())
            .await?;
        transaction
            .execute(
                "CREATE TABLE accepted_content_pin (
                    pin_id BLOB PRIMARY KEY CHECK (length(pin_id) = 16),
                    repository_id BLOB NOT NULL CHECK (length(repository_id) = 32),
                    database_id BLOB NOT NULL CHECK (length(database_id) = 16),
                    descriptor_oid_algorithm INTEGER NOT NULL CHECK (descriptor_oid_algorithm IN (1, 2)),
                    descriptor_oid BLOB NOT NULL CHECK (
                        (descriptor_oid_algorithm = 1 AND length(descriptor_oid) = 20) OR
                        (descriptor_oid_algorithm = 2 AND length(descriptor_oid) = 32)
                    ),
                    content_length INTEGER NOT NULL CHECK (content_length >= 0),
                    content_sha256 BLOB NOT NULL CHECK (length(content_sha256) = 32),
                    owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
                    owner_epoch INTEGER NOT NULL CHECK (owner_epoch > 0),
                    checkpoint_generation INTEGER CHECK (
                        checkpoint_generation IS NULL OR checkpoint_generation > 0
                    ),
                    FOREIGN KEY (checkpoint_generation) REFERENCES checkpoint(generation)
                )",
                (),
            )
            .await
            ?;
    } else if !has_checkpoint_generation {
        if active_count != 0 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        transaction
            .execute(
                "ALTER TABLE accepted_content_pin
                 ADD COLUMN checkpoint_generation INTEGER
                 REFERENCES checkpoint(generation)",
                (),
            )
            .await?;
    }
    let mut null_generation_rows = transaction
        .query(
            "SELECT COUNT(*) FROM accepted_content_pin WHERE checkpoint_generation IS NULL",
            (),
        )
        .await?;
    let null_generation_count = null_generation_rows
        .next()
        .await?
        .ok_or(RuntimeError::RecoveryInvalid)?
        .get::<i64>(0)
        .map_err(|_| RuntimeError::RecoveryInvalid)?;
    drop(null_generation_rows);
    if null_generation_count != 0 {
        return Err(RuntimeError::RecoveryInvalid);
    }

    let mut checkpoint_columns = transaction
        .query("PRAGMA table_info(checkpoint)", ())
        .await?;
    let mut has_pin_manifest = false;
    while let Some(row) = checkpoint_columns.next().await? {
        let column: String = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
        has_pin_manifest |= column == "accepted_pin_manifest_digest";
    }
    drop(checkpoint_columns);
    if !has_pin_manifest {
        if active_count != 0 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        transaction
            .execute(
                "ALTER TABLE checkpoint
                 ADD COLUMN accepted_pin_manifest_digest BLOB NOT NULL
                 DEFAULT X'0000000000000000000000000000000000000000000000000000000000000000'
                 CHECK (length(accepted_pin_manifest_digest) = 32)",
                (),
            )
            .await?;
    }
    transaction
        .execute(
            "INSERT OR IGNORE INTO runtime_schema_migration (migration)
             VALUES ('accepted-content-pin-checkpoint-integrity-v2')",
            (),
        )
        .await?;
    transaction.commit().await.map_err(RuntimeError::from)
}

impl RuntimeState {
    pub(crate) async fn initialize_runtime_meta(
        connection: &Connection,
        identity: RuntimeIdentity,
        digest: [u8; 32],
    ) -> Result<(), RuntimeError> {
        let runtime_id = *Uuid::new_v4().as_bytes();
        connection.execute(
            "INSERT INTO runtime_meta (singleton, database_id, repository_id, runtime_id, generation, generation_digest)
             VALUES (1, ?1, ?2, ?3, 0, ?4) ON CONFLICT(singleton) DO NOTHING",
            params![identity.database_id.to_vec(), identity.repository_id.to_vec(), runtime_id.to_vec(), digest.to_vec()],
        ).await?;
        let mut rows = connection
            .query(
                "SELECT database_id, repository_id FROM runtime_meta WHERE singleton = 1",
                (),
            )
            .await?;
        let row = rows.next().await?.ok_or(RuntimeError::RecoveryInvalid)?;
        let stored = RuntimeIdentity {
            database_id: fixed(row.get(0).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
            repository_id: fixed(row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?)?,
        };
        if stored != identity {
            return Err(RuntimeError::InvalidIdentity);
        }
        Ok(())
    }

    pub(crate) async fn migrate_stream_failure_payloads(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let applied = transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('stream-failure-payload-v1')",
                (),
            )
            .await?;
        if applied == 1 {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO stream_failure_payload_legacy (identity_id)
                     SELECT failure.identity_id
                     FROM stream_failure AS failure
                     LEFT JOIN stream_failure_payload AS payload
                       ON payload.identity_id = failure.identity_id
                     WHERE payload.identity_id IS NULL",
                    (),
                )
                .await?;
        }
        transaction.commit().await.map_err(RuntimeError::from)
    }

    /// Marks the additive assertion-detail relation as available. Existing
    /// code/class-only failure rows deliberately remain readable with no
    /// companion detail row.
    pub(crate) async fn migrate_stream_failure_assertion_details(
        &self,
    ) -> Result<(), RuntimeError> {
        self.connection
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('stream-failure-assertion-detail-v1')",
                (),
            )
            .await?;
        Ok(())
    }

    /// Upgrades the private stream identity store so its partition component
    /// has the same nullable semantics as the public checkpoint and stream
    /// natural keys. Existing non-null rows retain their byte-for-byte key
    /// identifiers; only the SQL column constraint changes.
    pub(crate) async fn migrate_nullable_stream_partitions(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let checkpoint_partition_required =
            stream_partition_required(&transaction, "stream_checkpoint").await?;
        let failure_partition_required =
            stream_partition_required(&transaction, "stream_failure").await?;
        let failure_code_requires_upgrade =
            stream_failure_code_requires_upgrade(&transaction).await?;
        if checkpoint_partition_required
            || failure_partition_required
            || failure_code_requires_upgrade
        {
            transaction
                .execute_batch(
                    "CREATE TABLE stream_checkpoint_nullable_partition (
                        key_id TEXT PRIMARY KEY CHECK (length(key_id) > 0),
                        consumer_principal TEXT NOT NULL CHECK (length(consumer_principal) > 0),
                        consumer_root TEXT NOT NULL CHECK (length(consumer_root) > 0),
                        consumer_function TEXT NOT NULL CHECK (length(consumer_function) > 0),
                        consumer_binding TEXT NOT NULL CHECK (length(consumer_binding) > 0),
                        source_format TEXT NOT NULL CHECK (length(source_format) > 0),
                        source TEXT NOT NULL CHECK (length(source) > 0),
                        partition_format TEXT NOT NULL CHECK (length(partition_format) > 0),
                        partition TEXT CHECK (partition IS NULL OR length(partition) > 0),
                        position_format TEXT NOT NULL CHECK (length(position_format) > 0),
                        version INTEGER NOT NULL CHECK (version >= 0),
                        committed_position TEXT,
                        next_fence INTEGER NOT NULL CHECK (next_fence >= 0)
                    );
                    CREATE TABLE stream_failure_nullable_partition (
                        identity_id TEXT PRIMARY KEY CHECK (length(identity_id) > 0),
                        key_id TEXT NOT NULL CHECK (length(key_id) > 0),
                        consumer_principal TEXT NOT NULL CHECK (length(consumer_principal) > 0),
                        consumer_root TEXT NOT NULL CHECK (length(consumer_root) > 0),
                        consumer_function TEXT NOT NULL CHECK (length(consumer_function) > 0),
                        consumer_binding TEXT NOT NULL CHECK (length(consumer_binding) > 0),
                        source_format TEXT NOT NULL CHECK (length(source_format) > 0),
                        source TEXT NOT NULL CHECK (length(source) > 0),
                        partition_format TEXT NOT NULL CHECK (length(partition_format) > 0),
                        partition TEXT CHECK (partition IS NULL OR length(partition) > 0),
                        position_format TEXT NOT NULL CHECK (length(position_format) > 0),
                        delivery_position TEXT NOT NULL CHECK (length(delivery_position) > 0),
                        successor_position TEXT NOT NULL CHECK (length(successor_position) > 0),
                        version INTEGER NOT NULL CHECK (version >= 0),
                        attempts INTEGER NOT NULL CHECK (attempts >= 0),
                        status INTEGER NOT NULL CHECK (status BETWEEN 1 AND 7),
                        diagnostic_code INTEGER NOT NULL CHECK (diagnostic_code BETWEEN 1 AND 6),
                        diagnostic_class INTEGER NOT NULL CHECK (diagnostic_class BETWEEN 1 AND 3)
                    );
                    INSERT INTO stream_checkpoint_nullable_partition
                        SELECT * FROM stream_checkpoint;
                    INSERT INTO stream_failure_nullable_partition
                        SELECT * FROM stream_failure;
                    DROP TABLE stream_checkpoint;
                    DROP TABLE stream_failure;
                    ALTER TABLE stream_checkpoint_nullable_partition RENAME TO stream_checkpoint;
                    ALTER TABLE stream_failure_nullable_partition RENAME TO stream_failure;",
                )
                .await?;
        }
        let mut empty_partitions = transaction
            .query(
                "SELECT COUNT(*) FROM sys_stream_observation WHERE partition = ''",
                (),
            )
            .await?;
        let empty_partition_count = empty_partitions
            .next()
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?
            .get::<i64>(0)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        if empty_partition_count != 0 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        transaction
            .execute_batch(
                "DROP INDEX IF EXISTS sys_stream_observation_natural_key;
                 CREATE UNIQUE INDEX IF NOT EXISTS sys_stream_observation_null_natural_key
                   ON sys_stream_observation (run_id, source_identity)
                   WHERE partition IS NULL;
                 CREATE UNIQUE INDEX IF NOT EXISTS sys_stream_observation_present_natural_key
                   ON sys_stream_observation (run_id, source_identity, partition)
                   WHERE partition IS NOT NULL;",
            )
            .await?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('nullable-stream-partition-v1')",
                (),
            )
            .await?;
        transaction.commit().await.map_err(RuntimeError::from)
    }

    /// Admits the empty text key `""` (VFS-1 `~empty`). Earlier stores carry a
    /// `length(row_key) > 0` CHECK on `table_row` and `runtime_table_history`,
    /// which SQLite cannot drop in place, so both tables are rebuilt in one
    /// transaction. Rows keep their keys and values; the rebuild runs only while
    /// the legacy CHECK is still present.
    pub(crate) async fn migrate_empty_row_keys(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let mut legacy = transaction
            .query(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table'
                   AND name IN ('table_row', 'runtime_table_history')
                   AND sql LIKE '%length(row_key) > 0%'",
                (),
            )
            .await?;
        let legacy_count = legacy
            .next()
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?
            .get::<i64>(0)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        drop(legacy);
        if legacy_count != 0 {
            transaction
                .execute_batch(
                    "CREATE TABLE table_row_empty_key (
                        table_id TEXT NOT NULL CHECK (length(table_id) > 0),
                        row_key BLOB NOT NULL,
                        row_value BLOB NOT NULL,
                        row_digest BLOB NOT NULL CHECK (length(row_digest) = 32),
                        PRIMARY KEY (table_id, row_key)
                    );
                    INSERT INTO table_row_empty_key (table_id, row_key, row_value, row_digest)
                        SELECT table_id, row_key, row_value, row_digest FROM table_row;
                    DROP TABLE table_row;
                    ALTER TABLE table_row_empty_key RENAME TO table_row;
                    CREATE TABLE runtime_table_history_empty_key (
                        mutation_sequence INTEGER NOT NULL CHECK (mutation_sequence >= 0),
                        table_id TEXT NOT NULL CHECK (length(table_id) > 0),
                        row_key BLOB NOT NULL,
                        row_value BLOB,
                        row_digest BLOB,
                        deleted INTEGER NOT NULL CHECK (deleted IN (0, 1)),
                        PRIMARY KEY (mutation_sequence, table_id, row_key),
                        CHECK (
                            (deleted = 1 AND row_value IS NULL AND row_digest IS NULL)
                            OR (deleted = 0 AND row_value IS NOT NULL AND length(row_digest) = 32)
                        )
                    );
                    INSERT INTO runtime_table_history_empty_key
                        (mutation_sequence, table_id, row_key, row_value, row_digest, deleted)
                        SELECT mutation_sequence, table_id, row_key, row_value, row_digest, deleted
                        FROM runtime_table_history;
                    DROP TABLE runtime_table_history;
                    ALTER TABLE runtime_table_history_empty_key RENAME TO runtime_table_history;
                    CREATE INDEX IF NOT EXISTS runtime_table_history_lookup
                        ON runtime_table_history (table_id, row_key, mutation_sequence);",
                )
                .await?;
            transaction
                .execute(
                    "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                     VALUES ('empty-row-key-v1')",
                    (),
                )
                .await?;
        }
        transaction.commit().await.map_err(RuntimeError::from)
    }

    /// Makes the admin audit's `observed_generation` nullable, so a failed
    /// invocation whose CWD cannot be read is still recorded as
    /// `failure:unobservable`. The audit table and its checkpoint-result child
    /// are rebuilt in one transaction; existing rows keep their values.
    pub(crate) async fn migrate_admin_failure_unobservable(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let mut columns = transaction
            .query("PRAGMA table_info(admin_invocation_audit)", ())
            .await?;
        let mut generation_required = None;
        while let Some(column) = columns.next().await? {
            let name: String = column.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
            if name == "observed_generation" {
                let notnull: i64 = column.get(3).map_err(|_| RuntimeError::RecoveryInvalid)?;
                generation_required = Some(notnull == 1);
            }
        }
        if generation_required.ok_or(RuntimeError::RecoveryInvalid)? {
            transaction
                .execute_batch(
                    "CREATE TABLE admin_invocation_audit_nullable (
                        sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                        invocation_id BLOB NOT NULL UNIQUE CHECK (length(invocation_id) = 16),
                        function_name TEXT NOT NULL CHECK (length(function_name) > 0),
                        safe_arguments TEXT NOT NULL CHECK (length(safe_arguments) <= 16777216),
                        owner_id BLOB NOT NULL CHECK (length(owner_id) = 16),
                        owner_epoch INTEGER NOT NULL CHECK (owner_epoch > 0),
                        observed_generation INTEGER CHECK (observed_generation IS NULL OR observed_generation >= 0),
                        terminal_outcome TEXT NOT NULL CHECK (length(terminal_outcome) > 0 AND length(terminal_outcome) <= 16777216),
                        succeeded INTEGER NOT NULL CHECK (succeeded IN (0, 1)),
                        redacted INTEGER NOT NULL CHECK (redacted IN (0, 1))
                    );
                    CREATE TABLE admin_invocation_checkpoint_result_nullable (
                        invocation_id BLOB PRIMARY KEY REFERENCES admin_invocation_audit_nullable(invocation_id),
                        version INTEGER NOT NULL CHECK (version >= 0),
                        committed_position TEXT NOT NULL CHECK (length(committed_position) > 0)
                    );
                    INSERT INTO admin_invocation_audit_nullable SELECT * FROM admin_invocation_audit;
                    INSERT INTO admin_invocation_checkpoint_result_nullable
                        SELECT * FROM admin_invocation_checkpoint_result;
                    DROP TABLE admin_invocation_checkpoint_result;
                    DROP TABLE admin_invocation_audit;
                    ALTER TABLE admin_invocation_audit_nullable RENAME TO admin_invocation_audit;
                    ALTER TABLE admin_invocation_checkpoint_result_nullable
                        RENAME TO admin_invocation_checkpoint_result;",
                )
                .await
                ?;
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('admin-failure-unobservable-v1')",
                (),
            )
            .await?;
        transaction.commit().await.map_err(RuntimeError::from)
    }

    /// Upgrades the runtime catalogue kind domain to include committed module
    /// identities. The dependency graph is rebuilt inside one transaction
    /// with foreign keys enabled; all old rows are copied before the old
    /// tables are replaced, and both pre- and post-rebuild FK checks must be
    /// clean before the migration marker can commit.
    pub(crate) async fn migrate_catalogue_identity_schema(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let mut pragma = transaction.query("PRAGMA foreign_keys", ()).await?;
        let foreign_keys = pragma
            .next()
            .await?
            .ok_or(RuntimeError::RecoveryInvalid)?
            .get::<i64>(0)
            .map_err(|_| RuntimeError::RecoveryInvalid)?;
        drop(pragma);
        if foreign_keys != 1 {
            return Err(RuntimeError::RecoveryInvalid);
        }
        let mut rows = transaction
            .query(
                "SELECT sql FROM sqlite_master
                 WHERE type = 'table' AND name = 'runtime_catalogue_identity'",
                (),
            )
            .await?;
        let identity_sql: Option<String> = rows.next().await?.map(|row| row.get(0)).transpose()?;
        drop(rows);
        if identity_sql.is_some_and(|sql| sql.contains("IN (1, 2, 3)")) {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                     VALUES ('runtime-catalogue-identity-v2')",
                    (),
                )
                .await?;
            return transaction.commit().await.map_err(RuntimeError::from);
        }

        Self::assert_catalogue_foreign_keys_tx(&transaction).await?;
        transaction
            .execute_batch(
                "CREATE TABLE runtime_catalogue_identity_v2 (
                     object_id BLOB PRIMARY KEY CHECK (length(object_id) = 16),
                     kind INTEGER NOT NULL CHECK (kind IN (1, 2, 3)),
                     created_ms INTEGER NOT NULL
                 );
                 CREATE TABLE runtime_catalogue_revision_v2 (
                     object_id BLOB NOT NULL REFERENCES runtime_catalogue_identity_v2(object_id),
                     snapshot BLOB NOT NULL CHECK (length(snapshot) > 0),
                     kind INTEGER NOT NULL CHECK (kind IN (1, 2, 3)),
                     qualified_name TEXT NOT NULL CHECK (length(qualified_name) > 0),
                     revision_id BLOB NOT NULL CHECK (length(revision_id) = 32),
                     semantic_hash BLOB NOT NULL CHECK (length(semantic_hash) = 32),
                     PRIMARY KEY (object_id, snapshot),
                     UNIQUE (snapshot, qualified_name)
                 );
                 CREATE TABLE runtime_catalogue_module_admission_v2 (
                     object_id BLOB NOT NULL REFERENCES runtime_catalogue_identity_v2(object_id),
                     snapshot BLOB NOT NULL REFERENCES runtime_catalogue_capture(snapshot),
                     PRIMARY KEY (object_id, snapshot)
                 );
                 CREATE TABLE runtime_catalogue_type_v2 (
                     object_id BLOB NOT NULL,
                     snapshot BLOB NOT NULL,
                     form TEXT NOT NULL CHECK (form IN ('named', 'value', 'reference')),
                     target_object_id BLOB,
                     PRIMARY KEY (object_id, snapshot),
                     FOREIGN KEY (object_id, snapshot)
                         REFERENCES runtime_catalogue_revision_v2(object_id, snapshot),
                     CHECK ((form = 'reference') = (target_object_id IS NOT NULL)),
                     CHECK (target_object_id IS NULL OR length(target_object_id) = 16)
                 );
                 CREATE TABLE runtime_catalogue_function_v2 (
                     object_id BLOB NOT NULL,
                     snapshot BLOB NOT NULL,
                     result_type_object_id BLOB NOT NULL CHECK (length(result_type_object_id) = 16),
                     PRIMARY KEY (object_id, snapshot),
                     FOREIGN KEY (object_id, snapshot)
                         REFERENCES runtime_catalogue_revision_v2(object_id, snapshot)
                 );
                 CREATE TABLE runtime_catalogue_parameter_v2 (
                     object_id BLOB NOT NULL,
                     snapshot BLOB NOT NULL,
                     position INTEGER NOT NULL CHECK (position >= 0),
                     name TEXT NOT NULL CHECK (length(name) > 0),
                     type_object_id BLOB NOT NULL CHECK (length(type_object_id) = 16),
                     PRIMARY KEY (object_id, snapshot, position),
                     FOREIGN KEY (object_id, snapshot)
                         REFERENCES runtime_catalogue_function_v2(object_id, snapshot)
                 );
                 INSERT INTO runtime_catalogue_identity_v2
                     SELECT object_id, kind, created_ms FROM runtime_catalogue_identity;
                 INSERT INTO runtime_catalogue_revision_v2
                     SELECT object_id, snapshot, kind, qualified_name, revision_id, semantic_hash
                     FROM runtime_catalogue_revision;
                 INSERT INTO runtime_catalogue_module_admission_v2
                     SELECT object_id, snapshot FROM runtime_catalogue_module_admission;
                 INSERT INTO runtime_catalogue_type_v2
                     SELECT object_id, snapshot, form, target_object_id
                     FROM runtime_catalogue_type;
                 INSERT INTO runtime_catalogue_function_v2
                     SELECT object_id, snapshot, result_type_object_id
                     FROM runtime_catalogue_function;
                 INSERT INTO runtime_catalogue_parameter_v2
                     SELECT object_id, snapshot, position, name, type_object_id
                     FROM runtime_catalogue_parameter;
                 DROP TABLE runtime_catalogue_module_admission;
                 DROP TABLE runtime_catalogue_parameter;
                 DROP TABLE runtime_catalogue_function;
                 DROP TABLE runtime_catalogue_type;
                 DROP TABLE runtime_catalogue_revision;
                 DROP TABLE runtime_catalogue_identity;
                 ALTER TABLE runtime_catalogue_identity_v2 RENAME TO runtime_catalogue_identity;
                 ALTER TABLE runtime_catalogue_revision_v2 RENAME TO runtime_catalogue_revision;
                 ALTER TABLE runtime_catalogue_module_admission_v2
                     RENAME TO runtime_catalogue_module_admission;
                 ALTER TABLE runtime_catalogue_type_v2 RENAME TO runtime_catalogue_type;
                 ALTER TABLE runtime_catalogue_function_v2 RENAME TO runtime_catalogue_function;
                 ALTER TABLE runtime_catalogue_parameter_v2 RENAME TO runtime_catalogue_parameter;",
            )
            .await?;
        Self::assert_catalogue_foreign_keys_tx(&transaction).await?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('runtime-catalogue-identity-v2')",
                (),
            )
            .await?;
        transaction.commit().await.map_err(RuntimeError::from)
    }

    async fn assert_catalogue_foreign_keys_tx(
        transaction: &libsql::Transaction,
    ) -> Result<(), RuntimeError> {
        let mut rows = transaction.query("PRAGMA foreign_key_check", ()).await?;
        if rows.next().await?.is_some() {
            return Err(RuntimeError::RecoveryInvalid);
        }
        Ok(())
    }

    /// Adds the durable run observation timestamp without reinterpreting any
    /// retained request, lease, or recovery state. Older rows use their
    /// terminal instant when present, otherwise their admission instant.
    pub(crate) async fn migrate_observation_projection_schema(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let mut rows = transaction
            .query("PRAGMA table_info(sys_run_observation)", ())
            .await?;
        let mut columns = BTreeMap::new();
        while let Some(row) = rows.next().await? {
            columns.insert(
                row.get::<String>(1)
                    .map_err(|_| RuntimeError::RecoveryInvalid)?,
                (),
            );
        }
        if !columns.contains_key("observed_ms") {
            transaction
                .execute(
                    "ALTER TABLE sys_run_observation
                     ADD COLUMN observed_ms INTEGER NOT NULL DEFAULT 0",
                    (),
                )
                .await?;
            transaction
                .execute(
                    "UPDATE sys_run_observation
                     SET observed_ms = COALESCE(ended_ms, started_ms)",
                    (),
                )
                .await?;
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('observation-projection-v1')",
                (),
            )
            .await?;
        transaction.commit().await.map_err(RuntimeError::from)
    }

    /// Separates retained per-run stream observations from durable checkpoint
    /// identity. Older databases made `checkpoint_key_id` globally unique,
    /// preventing a completed run from being observed after restart.
    pub(crate) async fn migrate_stream_observation_checkpoint_reuse(
        &self,
    ) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let applied = transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('stream-observation-checkpoint-reuse-v1')",
                (),
            )
            .await?;
        if applied == 0 {
            return transaction.commit().await.map_err(RuntimeError::from);
        }
        transaction
            .execute_batch(
                "CREATE TABLE sys_stream_observation_reusable_checkpoint (
                    stream_id BLOB PRIMARY KEY CHECK (length(stream_id) = 16),
                    run_id BLOB NOT NULL REFERENCES sys_run_observation(run_id),
                    checkpoint_key_id TEXT NOT NULL CHECK (length(checkpoint_key_id) > 0),
                    producer TEXT NOT NULL CHECK (length(producer) > 0),
                    consumer_name TEXT,
                    consumer_identity TEXT NOT NULL CHECK (length(consumer_identity) > 0),
                    source_identity TEXT NOT NULL CHECK (length(source_identity) > 0),
                    partition TEXT CHECK (partition IS NULL OR length(partition) > 0),
                    status INTEGER NOT NULL CHECK (status BETWEEN 1 AND 8),
                    items_seen INTEGER NOT NULL DEFAULT 0 CHECK (items_seen >= 0),
                    items_committed INTEGER NOT NULL DEFAULT 0 CHECK (items_committed >= 0),
                    items_failed INTEGER NOT NULL DEFAULT 0 CHECK (items_failed >= 0),
                    checkpoint_version INTEGER,
                    last_failure_identity TEXT,
                    last_item_ms INTEGER,
                    diagnostic_code INTEGER,
                    diagnostic_class INTEGER,
                    observed_ms INTEGER NOT NULL,
                    UNIQUE(run_id, source_identity, partition),
                    CHECK ((diagnostic_code IS NULL) = (diagnostic_class IS NULL))
                );
                INSERT INTO sys_stream_observation_reusable_checkpoint
                    SELECT * FROM sys_stream_observation;
                DROP TABLE sys_stream_observation;
                ALTER TABLE sys_stream_observation_reusable_checkpoint
                    RENAME TO sys_stream_observation;
                CREATE UNIQUE INDEX sys_stream_observation_null_natural_key
                    ON sys_stream_observation (run_id, source_identity)
                    WHERE partition IS NULL;
                CREATE UNIQUE INDEX sys_stream_observation_present_natural_key
                    ON sys_stream_observation (run_id, source_identity, partition)
                    WHERE partition IS NOT NULL;",
            )
            .await?;
        transaction.commit().await.map_err(RuntimeError::from)
    }

    /// Adds the causal link from a retained stream observation to its most
    /// recent durable delivery-failure identity. Existing rows intentionally
    /// remain null: no historical failure is inferred without exact evidence.
    pub(crate) async fn migrate_stream_observation_failure_identity(
        &self,
    ) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let mut rows = transaction
            .query("PRAGMA table_info(sys_stream_observation)", ())
            .await?;
        let mut found = false;
        while let Some(row) = rows.next().await? {
            if row
                .get::<String>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?
                == "last_failure_identity"
            {
                found = true;
            }
        }
        if !found {
            transaction
                .execute(
                    "ALTER TABLE sys_stream_observation ADD COLUMN last_failure_identity TEXT",
                    (),
                )
                .await?;
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('stream-observation-failure-identity-v1')",
                (),
            )
            .await?;
        transaction.commit().await.map_err(RuntimeError::from)
    }

    /// Adds only backward-compatible ledger columns. The migration marker is
    /// transactional so an interrupted upgrade cannot look complete.
    pub(crate) async fn migrate_request_recovery_evidence(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let applied = transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('request-recovery-evidence-v2')",
                (),
            )
            .await?;
        if applied == 1 {
            let mut rows = transaction
                .query("PRAGMA table_info(request_ledger)", ())
                .await?;
            let mut columns = BTreeMap::new();
            while let Some(row) = rows.next().await? {
                let name: String = row.get(1).map_err(|_| RuntimeError::RecoveryInvalid)?;
                columns.insert(name, ());
            }
            for (column, sql) in [
                (
                    "owner_id",
                    "ALTER TABLE request_ledger ADD COLUMN owner_id BLOB",
                ),
                (
                    "owner_epoch",
                    "ALTER TABLE request_ledger ADD COLUMN owner_epoch INTEGER",
                ),
                (
                    "effect_evidence",
                    "ALTER TABLE request_ledger ADD COLUMN effect_evidence INTEGER NOT NULL DEFAULT 0",
                ),
                (
                    "recovery_disposition",
                    "ALTER TABLE request_ledger ADD COLUMN recovery_disposition INTEGER NOT NULL DEFAULT 0",
                ),
                (
                    "controlled_transaction_proof",
                    "ALTER TABLE request_ledger ADD COLUMN controlled_transaction_proof BLOB",
                ),
                (
                    "controlled_rollback_proof",
                    "ALTER TABLE request_ledger ADD COLUMN controlled_rollback_proof BLOB",
                ),
            ] {
                if !columns.contains_key(column) {
                    transaction.execute(sql, ()).await?;
                }
            }
        }
        transaction.commit().await.map_err(RuntimeError::from)
    }

    /// Adds the runtime owner lock identity a writer lease is bound to. A
    /// lease written before this column existed stays unbound (null): no
    /// live lock vouches for its holder, so only the worktree's current lock
    /// holder may fence it.
    pub(crate) async fn migrate_writer_lease_lock_binding(&self) -> Result<(), RuntimeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .await?;
        let mut rows = transaction
            .query("PRAGMA table_info(writer_lease)", ())
            .await?;
        let mut found = false;
        while let Some(row) = rows.next().await? {
            if row
                .get::<String>(1)
                .map_err(|_| RuntimeError::RecoveryInvalid)?
                == "lock_owner_id"
            {
                found = true;
            }
        }
        if !found {
            transaction
                .execute(
                    "ALTER TABLE writer_lease ADD COLUMN lock_owner_id BLOB
                     CHECK (lock_owner_id IS NULL OR length(lock_owner_id) = 16)",
                    (),
                )
                .await?;
        }
        transaction
            .execute(
                "INSERT OR IGNORE INTO runtime_schema_migration (migration)
                 VALUES ('writer-lease-lock-binding-v1')",
                (),
            )
            .await?;
        transaction.commit().await.map_err(RuntimeError::from)
    }
}
