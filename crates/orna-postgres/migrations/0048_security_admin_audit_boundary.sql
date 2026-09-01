-- Reapply the closed SecurityAdmin audit boundary for databases that applied
-- migration 0028 before its durable detail constraint was added. Current
-- databases already carry the same constraint; replacing it keeps the repair
-- idempotent across both migration histories without changing the v28 bytes.
ALTER TABLE _orna_kernel.security_audit_events
    DROP CONSTRAINT IF EXISTS security_audit_events_security_admin_detail_check,
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
