-- Source-apply audit rows are emitted only by the installed catalogue-health host.
-- Keep the principal binding in protected storage as well as in host code.

ALTER TABLE _orna_kernel.security_audit_events
    ADD CONSTRAINT security_audit_events_source_apply_principal_check CHECK (
        event_kind <> 'source_apply'
        OR session_principal_id = decode('00000000000000000000000000000001', 'hex')
    );
