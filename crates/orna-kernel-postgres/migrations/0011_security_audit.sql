CREATE TABLE _orna_kernel.security_audit_events (
    sequence bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_id bytea NOT NULL,
    recorded_at timestamp NOT NULL
        DEFAULT (transaction_timestamp() AT TIME ZONE 'UTC'),
    event_kind text NOT NULL,
    outcome text NOT NULL,
    session_principal_id bytea,
    effective_principal_id bytea,
    authorising_principal_id bytea,
    function_id bytea,
    source_revision_id bytea,
    catalogue_revision_id bytea,
    denial_reason text,
    CONSTRAINT security_audit_events_event_id_key UNIQUE (event_id),
    CONSTRAINT security_audit_events_identity_lengths CHECK (
        octet_length(event_id) = 16
        AND (session_principal_id IS NULL OR octet_length(session_principal_id) = 16)
        AND (effective_principal_id IS NULL OR octet_length(effective_principal_id) = 16)
        AND (authorising_principal_id IS NULL OR octet_length(authorising_principal_id) = 16)
        AND (function_id IS NULL OR octet_length(function_id) = 16)
        AND (source_revision_id IS NULL OR octet_length(source_revision_id) = 16)
        AND (catalogue_revision_id IS NULL OR octet_length(catalogue_revision_id) = 16)
    ),
    CONSTRAINT security_audit_events_kind_check
        CHECK (event_kind IN ('authentication', 'execute')),
    CONSTRAINT security_audit_events_outcome_check
        CHECK (outcome IN ('allowed', 'denied')),
    CONSTRAINT security_audit_events_revision_pair_check CHECK (
        (source_revision_id IS NULL) = (catalogue_revision_id IS NULL)
    ),
    CONSTRAINT security_audit_events_denial_reason_check CHECK (
        denial_reason IS NULL OR denial_reason IN (
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
    ),
    CONSTRAINT security_audit_events_shape_check CHECK (
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
        )
    )
);

REVOKE ALL ON TABLE _orna_kernel.security_audit_events FROM PUBLIC;
REVOKE ALL ON SEQUENCE _orna_kernel.security_audit_events_sequence_seq FROM PUBLIC;
