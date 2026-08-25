-- ADR 0065: the durable privilege-class grant model backs the protected
-- sys.security admin surface. One row per grant of one closed privilege
-- class to one grantee; the object identity is a sealed system function or
-- application function identity, and a class-wide grant stores the empty
-- bytea sentinel ('' ) so the composite primary key stays total (PostgreSQL
-- treats NULLs as distinct in unique keys, so a nullable object would admit
-- duplicate class-wide grants). The privilege class is the canonical closed
-- display set from the core model: execute, security_admin, and the seven
-- sealed INSPECT sub-privileges. The relation is private to _orna_kernel
-- with no public grants.

CREATE TABLE _orna_kernel.security_privilege_grants (
    grantee_id bytea NOT NULL,
    privilege_class text NOT NULL,
    object_id bytea NOT NULL,
    CONSTRAINT security_privilege_grants_pkey
        PRIMARY KEY (grantee_id, privilege_class, object_id),
    CONSTRAINT security_privilege_grants_grantee_length
        CHECK (octet_length(grantee_id) = 16),
    CONSTRAINT security_privilege_grants_object_length
        CHECK (object_id = '' OR octet_length(object_id) = 16),
    CONSTRAINT security_privilege_grants_class_check
        CHECK (
            privilege_class IN ('execute', 'security_admin')
            OR privilege_class IN (
                'inspect:own-invocation',
                'inspect:session-invocations',
                'inspect:any-invocation',
                'inspect:values',
                'inspect:source',
                'inspect:security-details',
                'inspect:runtime-internals'
            )
        ),
    CONSTRAINT security_privilege_grants_grantee_fk
        FOREIGN KEY (grantee_id)
        REFERENCES _orna_kernel.security_principals(id)
);

CREATE INDEX security_privilege_grants_object_index
    ON _orna_kernel.security_privilege_grants(object_id, grantee_id, privilege_class);

-- ADR 0065: protected audit admits closed security-admin decisions. The
-- detail records only the operation kind and the sealed target identity for
-- an allowed mutation, and the operation plus the closed missing-privilege
-- reason for a denied one; argument payloads are never written to audit.

ALTER TABLE _orna_kernel.security_audit_events
    DROP CONSTRAINT security_audit_events_kind_check,
    ADD CONSTRAINT security_audit_events_kind_check
        CHECK (event_kind IN (
            'authentication',
            'execute',
            'capability',
            'user_state',
            'inspect',
            'security_admin'
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
            'execute_missing_grant'
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
        )
    );

-- Security-admin audit rows carry one closed operation/detail pair and the
-- sealed system function that implements that operation. Recovery repeats this
-- mapping, but the durable boundary rejects forged vocabulary or targets too.
ALTER TABLE _orna_kernel.security_audit_events
    ADD CONSTRAINT security_audit_events_security_admin_detail_check CHECK (
        event_kind <> 'security_admin'
        OR (
            function_id IS NOT NULL
            AND denial_reason IS NOT NULL
            AND (
                (
                    function_id = decode('00000000000000000000000000000043', 'hex')
                    AND (
                        (outcome = 'allowed' AND denial_reason = 'security_admin:create_principal')
                        OR (outcome = 'denied' AND denial_reason = 'security_admin:create_principal:missing-privilege')
                    )
                )
                OR (
                    function_id = decode('00000000000000000000000000000044', 'hex')
                    AND (
                        (outcome = 'allowed' AND denial_reason = 'security_admin:disable_principal')
                        OR (outcome = 'denied' AND denial_reason = 'security_admin:disable_principal:missing-privilege')
                    )
                )
                OR (
                    function_id = decode('00000000000000000000000000000045', 'hex')
                    AND (
                        (outcome = 'allowed' AND denial_reason = 'security_admin:create_role')
                        OR (outcome = 'denied' AND denial_reason = 'security_admin:create_role:missing-privilege')
                    )
                )
                OR (
                    function_id = decode('00000000000000000000000000000046', 'hex')
                    AND (
                        (outcome = 'allowed' AND denial_reason = 'security_admin:grant_role')
                        OR (outcome = 'denied' AND denial_reason = 'security_admin:grant_role:missing-privilege')
                    )
                )
                OR (
                    function_id = decode('00000000000000000000000000000047', 'hex')
                    AND (
                        (outcome = 'allowed' AND denial_reason = 'security_admin:revoke_role')
                        OR (outcome = 'denied' AND denial_reason = 'security_admin:revoke_role:missing-privilege')
                    )
                )
                OR (
                    function_id = decode('00000000000000000000000000000048', 'hex')
                    AND (
                        (outcome = 'allowed' AND denial_reason = 'security_admin:grant_privilege')
                        OR (outcome = 'denied' AND denial_reason = 'security_admin:grant_privilege:missing-privilege')
                    )
                )
                OR (
                    function_id = decode('00000000000000000000000000000049', 'hex')
                    AND (
                        (outcome = 'allowed' AND denial_reason = 'security_admin:revoke_privilege')
                        OR (outcome = 'denied' AND denial_reason = 'security_admin:revoke_privilege:missing-privilege')
                    )
                )
            )
        )
    );

REVOKE ALL ON TABLE _orna_kernel.security_privilege_grants FROM PUBLIC;
