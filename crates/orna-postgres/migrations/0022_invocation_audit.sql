-- Retain one durable protected decision for an accepted sys.invoke operation.
-- This relation is decision evidence only. It does not retain request payloads
-- or invocation lifecycle, result, cancellation, or delivery state.

-- The composite key is the exact durable EXECUTE evidence that an invocation
-- decision may reference. The already-unique event identity keeps this
-- addition observational for existing protected audit history.
ALTER TABLE _orna_kernel.security_audit_events
    ADD CONSTRAINT security_audit_events_invocation_evidence_key
    UNIQUE (
        event_id,
        outcome,
        function_id,
        source_revision_id,
        catalogue_revision_id
    );

CREATE TABLE _orna_kernel.invocation_audit_events (
    sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_id bytea NOT NULL,
    recorded_at timestamp NOT NULL
        DEFAULT (transaction_timestamp() AT TIME ZONE 'UTC'),
    invocation_id bytea NOT NULL,
    outcome text NOT NULL,
    session_principal_id bytea NOT NULL,
    effective_principal_id bytea,
    authorising_principal_id bytea,
    function_id bytea,
    source_revision_id bytea,
    catalogue_revision_id bytea,
    security_audit_event_id bytea,
    CONSTRAINT invocation_audit_events_event_id_key UNIQUE (event_id),
    CONSTRAINT invocation_audit_events_invocation_id_key UNIQUE (invocation_id),
    CONSTRAINT invocation_audit_events_identity_lengths CHECK (
        octet_length(event_id) = 16
        AND octet_length(invocation_id) = 16
        AND octet_length(session_principal_id) = 16
        AND (effective_principal_id IS NULL OR octet_length(effective_principal_id) = 16)
        AND (authorising_principal_id IS NULL OR octet_length(authorising_principal_id) = 16)
        AND (function_id IS NULL OR octet_length(function_id) = 16)
        AND (source_revision_id IS NULL OR octet_length(source_revision_id) = 16)
        AND (catalogue_revision_id IS NULL OR octet_length(catalogue_revision_id) = 16)
        AND (security_audit_event_id IS NULL OR octet_length(security_audit_event_id) = 16)
    ),
    CONSTRAINT invocation_audit_events_outcome_check
        CHECK (outcome IN ('allowed', 'denied')),
    CONSTRAINT invocation_audit_events_principal_evidence_pair_check CHECK (
        (effective_principal_id IS NULL) = (authorising_principal_id IS NULL)
    ),
    CONSTRAINT invocation_audit_events_target_evidence_pair_check CHECK (
        (function_id IS NULL)
            = (source_revision_id IS NULL)
        AND (function_id IS NULL)
            = (catalogue_revision_id IS NULL)
        AND (function_id IS NULL)
            = (security_audit_event_id IS NULL)
    ),
    CONSTRAINT invocation_audit_events_outcome_shape_check CHECK (
        (outcome = 'allowed'
            AND function_id IS NOT NULL
            AND effective_principal_id IS NOT NULL)
        OR (outcome = 'denied')
    ),
    CONSTRAINT invocation_audit_events_target_fk
        FOREIGN KEY (catalogue_revision_id, function_id)
        REFERENCES _orna_kernel.catalogue_functions(catalogue_revision_id, function_id),
    CONSTRAINT invocation_audit_events_revision_pair_fk
        FOREIGN KEY (catalogue_revision_id, source_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(id, source_revision_id),
    CONSTRAINT invocation_audit_events_security_evidence_fk
        FOREIGN KEY (
            security_audit_event_id,
            outcome,
            function_id,
            source_revision_id,
            catalogue_revision_id
        )
        REFERENCES _orna_kernel.security_audit_events(
            event_id,
            outcome,
            function_id,
            source_revision_id,
            catalogue_revision_id
        )
);

REVOKE ALL ON TABLE _orna_kernel.invocation_audit_events FROM PUBLIC;
REVOKE ALL ON SEQUENCE _orna_kernel.invocation_audit_events_sequence_seq FROM PUBLIC;
