-- The installed source host records successful candidate activation using the
-- reserved catalogue-health service principal. Candidate source and catalogue
-- revisions reuse the existing pinned revision columns; no request payload or
-- new relation columns enter protected audit.

ALTER TABLE _orna_kernel.security_audit_events
    DROP CONSTRAINT security_audit_events_kind_check,
    ADD CONSTRAINT security_audit_events_kind_check
        CHECK (event_kind IN (
            'authentication',
            'execute',
            'capability',
            'user_state',
            'inspect',
            'security_admin',
            'source_apply'
        ));

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
            'execute_missing_grant',
            'source_apply:committed'
        )
        OR denial_reason LIKE 'capability:%'
        OR denial_reason LIKE 'user_state:%'
        OR denial_reason LIKE 'inspect:%'
        OR denial_reason LIKE 'security_admin:%'
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
        ) OR (
            event_kind = 'security_admin'
            AND outcome = 'allowed'
            AND session_principal_id IS NOT NULL
            AND effective_principal_id IS NULL
            AND authorising_principal_id IS NULL
            AND function_id IS NOT NULL
            AND source_revision_id IS NULL
            AND catalogue_revision_id IS NULL
            AND denial_reason IS NOT NULL
            AND denial_reason LIKE 'security_admin:%'
            AND denial_reason NOT LIKE '%:missing-privilege'
        ) OR (
            event_kind = 'security_admin'
            AND outcome = 'denied'
            AND session_principal_id IS NOT NULL
            AND effective_principal_id IS NULL
            AND authorising_principal_id IS NULL
            AND function_id IS NOT NULL
            AND source_revision_id IS NULL
            AND catalogue_revision_id IS NULL
            AND denial_reason IS NOT NULL
            AND denial_reason LIKE 'security_admin:%:missing-privilege'
        ) OR (
            event_kind = 'source_apply'
            AND outcome = 'allowed'
            AND session_principal_id IS NOT NULL
            AND effective_principal_id IS NULL
            AND authorising_principal_id IS NULL
            AND function_id IS NULL
            AND source_revision_id IS NOT NULL
            AND catalogue_revision_id IS NOT NULL
            AND denial_reason = 'source_apply:committed'
        )
    );
