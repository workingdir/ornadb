-- ADR 0078: retain one redacted terminal record for each authenticated resource.
-- This relation is lifecycle evidence, not a payload or result log.
CREATE TABLE _orna_kernel.resource_audit_events (
    sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_id bytea NOT NULL,
    recorded_at timestamp NOT NULL
        DEFAULT (transaction_timestamp() AT TIME ZONE 'UTC'),
    request_id bytea NOT NULL,
    nested_invocation_id bytea NOT NULL,
    parent_invocation_id bytea NOT NULL,
    call_site_id bytea NOT NULL,
    target_function_id bytea,
    source_revision_id bytea,
    catalogue_revision_id bytea,
    session_principal_id bytea NOT NULL,
    decision_outcome text NOT NULL,
    terminal_outcome text NOT NULL,
    item_count bigint,
    byte_count bigint,
    CONSTRAINT resource_audit_events_event_id_key UNIQUE (event_id),
    CONSTRAINT resource_audit_events_request_id_key UNIQUE (request_id),
    CONSTRAINT resource_audit_events_nested_invocation_id_key
        UNIQUE (nested_invocation_id),
    CONSTRAINT resource_audit_events_identity_lengths CHECK (
        octet_length(event_id) = 16
        AND octet_length(request_id) = 16
        AND octet_length(nested_invocation_id) = 16
        AND octet_length(parent_invocation_id) = 16
        AND octet_length(call_site_id) = 16
        AND octet_length(session_principal_id) = 16
        AND (target_function_id IS NULL OR octet_length(target_function_id) = 16)
        AND (source_revision_id IS NULL OR octet_length(source_revision_id) = 16)
        AND (catalogue_revision_id IS NULL OR octet_length(catalogue_revision_id) = 16)
    ),
    CONSTRAINT resource_audit_events_target_pair_check CHECK (
        (target_function_id IS NULL)
            = (source_revision_id IS NULL)
        AND (target_function_id IS NULL)
            = (catalogue_revision_id IS NULL)
    ),
    CONSTRAINT resource_audit_events_decision_outcome_check
        CHECK (decision_outcome IN ('allowed', 'denied')),
    CONSTRAINT resource_audit_events_terminal_outcome_check
        CHECK (terminal_outcome IN ('completed', 'failed', 'cancelled')),
    CONSTRAINT resource_audit_events_counts_check CHECK (
        (item_count IS NULL OR item_count >= 0)
        AND (byte_count IS NULL OR byte_count >= 0)
    ),
    CONSTRAINT resource_audit_events_target_fk
        FOREIGN KEY (catalogue_revision_id, target_function_id)
        REFERENCES _orna_kernel.catalogue_functions(catalogue_revision_id, function_id),
    CONSTRAINT resource_audit_events_revision_pair_fk
        FOREIGN KEY (catalogue_revision_id, source_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(id, source_revision_id),
    CONSTRAINT resource_audit_events_nested_invocation_fk
        FOREIGN KEY (nested_invocation_id)
        REFERENCES _orna_kernel.invocation_audit_events(invocation_id)
);

REVOKE ALL ON TABLE _orna_kernel.resource_audit_events FROM PUBLIC;
REVOKE ALL ON SEQUENCE _orna_kernel.resource_audit_events_sequence_seq FROM PUBLIC;
