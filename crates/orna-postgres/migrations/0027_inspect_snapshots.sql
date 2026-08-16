-- ADR 0064: the server-side Inspector core retains one immutable inspection
-- epoch per captured protected invocation and the sequence-addressable trace
-- of that invocation. Every capture is a new _orna_kernel.inspect_snapshots
-- row keyed by a fresh epoch id, pinned by the source and catalogue revision
-- pair active at capture time, holding the owner principal and the canonical
-- ORV5 epoch payload (summary_bytes). The trace relation retains one row per
-- closed stream event kind with the canonical ORV5 payload of that event.
-- Both relations are private to _orna_kernel with no public grants.

CREATE TABLE _orna_kernel.inspect_snapshots (
    epoch_id bytea NOT NULL,
    invocation_id bytea NOT NULL,
    recorded_at timestamp with time zone NOT NULL
        DEFAULT transaction_timestamp(),
    owner_principal_id bytea NOT NULL,
    source_revision_id bytea NOT NULL,
    catalogue_revision_id bytea NOT NULL,
    summary_bytes bytea NOT NULL,
    CONSTRAINT inspect_snapshots_pkey
        PRIMARY KEY (epoch_id),
    CONSTRAINT inspect_snapshots_identity_lengths CHECK (
        octet_length(epoch_id) = 16
        AND octet_length(invocation_id) = 16
        AND octet_length(owner_principal_id) = 16
        AND octet_length(source_revision_id) = 16
        AND octet_length(catalogue_revision_id) = 16
    ),
    CONSTRAINT inspect_snapshots_invocation_fk
        FOREIGN KEY (invocation_id)
        REFERENCES _orna_kernel.invocation_audit_events(invocation_id)
);

CREATE TABLE _orna_kernel.inspect_trace_events (
    invocation_id bytea NOT NULL,
    sequence bigint NOT NULL,
    kind text NOT NULL,
    payload_bytes bytea NOT NULL,
    observer_invocation_id bytea,
    recorded_at timestamp with time zone NOT NULL
        DEFAULT transaction_timestamp(),
    CONSTRAINT inspect_trace_events_pkey
        PRIMARY KEY (invocation_id, sequence),
    CONSTRAINT inspect_trace_events_identity_lengths CHECK (
        octet_length(invocation_id) = 16
        AND (observer_invocation_id IS NULL OR octet_length(observer_invocation_id) = 16)
    ),
    CONSTRAINT inspect_trace_events_sequence_check CHECK (sequence >= 0),
    CONSTRAINT inspect_trace_events_kind_check CHECK (
        kind IN (
            'started',
            'value_batch',
            'completed',
            'diagnostic',
            'inspect_snapshot',
            'inspect_projection',
            'inspect_trace',
            'security_decision'
        )
    ),
    CONSTRAINT inspect_trace_events_invocation_fk
        FOREIGN KEY (invocation_id)
        REFERENCES _orna_kernel.invocation_audit_events(invocation_id)
);

-- ADR 0064: protected audit admits closed INSPECT decisions. The detail
-- records only the closed requested privilege and epoch scope for an allowed
-- capture, and the closed denial reason for a denied inspection; typed epoch
-- content is never written to audit.

ALTER TABLE _orna_kernel.security_audit_events
    DROP CONSTRAINT security_audit_events_kind_check,
    ADD CONSTRAINT security_audit_events_kind_check
        CHECK (event_kind IN ('authentication', 'execute', 'capability', 'user_state', 'inspect'));

ALTER TABLE _orna_kernel.security_audit_events
    DROP CONSTRAINT security_audit_events_denial_reason_check,
    ADD CONSTRAINT security_audit_events_denial_reason_check CHECK (
        denial_reason IS NULL
        OR denial_reason IN (
            'authentication_unknown_uid',
            'authentication_unknown_session_principal',
            'authentication_disabled_session_principal',
            'authentication_role_cannot_authenticate',
            'authentication_duplicate_active_role',
            'authentication_unknown_active_role',
            'authentication_disabled_active_role',
            'authentication_active_principal_is_not_role',
            'authentication_unreachable_active_role',
            'execute_invalid_session',
            'execute_unknown_function',
            'execute_revision_mismatch',
            'execute_missing_grant'
        )
        OR denial_reason LIKE 'capability:%'
        OR denial_reason LIKE 'user_state:%'
        OR denial_reason LIKE 'inspect:%'
    );

ALTER TABLE _orna_kernel.security_audit_events
    DROP CONSTRAINT security_audit_events_shape_check,
    ADD CONSTRAINT security_audit_events_shape_check CHECK (
        (
            event_kind = 'authentication'
            AND outcome = 'allowed'
            AND session_principal_id IS NOT NULL
            AND effective_principal_id IS NULL
            AND authorising_principal_id IS NULL
            AND function_id IS NULL
            AND source_revision_id IS NULL
            AND catalogue_revision_id IS NULL
            AND denial_reason IS NULL
        ) OR (
            event_kind = 'authentication'
            AND outcome = 'denied'
            AND effective_principal_id IS NULL
            AND authorising_principal_id IS NULL
            AND function_id IS NULL
            AND source_revision_id IS NULL
            AND catalogue_revision_id IS NULL
            AND denial_reason IS NOT NULL
            AND denial_reason LIKE 'authentication_%'
            AND (
                (denial_reason = 'authentication_unknown_uid' AND session_principal_id IS NULL)
                OR (denial_reason <> 'authentication_unknown_uid' AND session_principal_id IS NOT NULL)
            )
        ) OR (
            event_kind = 'execute'
            AND outcome = 'allowed'
            AND session_principal_id IS NOT NULL
            AND effective_principal_id IS NOT NULL
            AND authorising_principal_id IS NOT NULL
            AND function_id IS NOT NULL
            AND source_revision_id IS NOT NULL
            AND catalogue_revision_id IS NOT NULL
            AND denial_reason IS NULL
        ) OR (
            event_kind = 'execute'
            AND outcome = 'denied'
            AND session_principal_id IS NOT NULL
            AND effective_principal_id IS NULL
            AND authorising_principal_id IS NULL
            AND function_id IS NOT NULL
            AND source_revision_id IS NOT NULL
            AND catalogue_revision_id IS NOT NULL
            AND denial_reason IS NOT NULL
            AND denial_reason LIKE 'execute_%'
        ) OR (
            event_kind = 'capability'
            AND session_principal_id IS NOT NULL
            AND effective_principal_id IS NULL
            AND authorising_principal_id IS NULL
            AND function_id IS NOT NULL
            AND source_revision_id IS NOT NULL
            AND catalogue_revision_id IS NOT NULL
            AND denial_reason IS NOT NULL
            AND denial_reason LIKE 'capability:%'
        ) OR (
            event_kind = 'user_state'
            AND outcome = 'allowed'
            AND session_principal_id IS NOT NULL
            AND effective_principal_id IS NULL
            AND authorising_principal_id IS NULL
            AND function_id IS NOT NULL
            AND source_revision_id IS NULL
            AND catalogue_revision_id IS NULL
            AND denial_reason IS NOT NULL
            AND denial_reason LIKE 'user_state:%'
        ) OR (
            event_kind = 'inspect'
            AND outcome = 'allowed'
            AND session_principal_id IS NOT NULL
            AND effective_principal_id IS NULL
            AND authorising_principal_id IS NULL
            AND function_id IS NULL
            AND source_revision_id IS NULL
            AND catalogue_revision_id IS NULL
            AND denial_reason IS NOT NULL
            AND denial_reason LIKE 'inspect:requested=%'
        ) OR (
            event_kind = 'inspect'
            AND outcome = 'denied'
            AND session_principal_id IS NOT NULL
            AND effective_principal_id IS NULL
            AND authorising_principal_id IS NULL
            AND function_id IS NULL
            AND source_revision_id IS NULL
            AND catalogue_revision_id IS NULL
            AND denial_reason IS NOT NULL
            AND denial_reason LIKE 'inspect:%'
        )
    );

REVOKE ALL ON TABLE _orna_kernel.inspect_snapshots FROM PUBLIC;
REVOKE ALL ON TABLE _orna_kernel.inspect_trace_events FROM PUBLIC;