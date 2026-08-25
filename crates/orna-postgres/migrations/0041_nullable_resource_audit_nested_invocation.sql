-- ADR 0078: requests denied or cancelled before RESOURCE_ACCEPTED have no
-- nested invocation identity. Accepted requests retain one non-null identity.
ALTER TABLE _orna_kernel.resource_audit_events
    ALTER COLUMN nested_invocation_id DROP NOT NULL;

ALTER TABLE _orna_kernel.resource_audit_events
    DROP CONSTRAINT resource_audit_events_identity_lengths,
    ADD CONSTRAINT resource_audit_events_identity_lengths CHECK (
        octet_length(event_id) = 16
        AND octet_length(request_id) = 16
        AND (nested_invocation_id IS NULL OR octet_length(nested_invocation_id) = 16)
        AND octet_length(parent_invocation_id) = 16
        AND octet_length(call_site_id) = 16
        AND octet_length(session_principal_id) = 16
        AND (target_function_id IS NULL OR octet_length(target_function_id) = 16)
        AND (source_revision_id IS NULL OR octet_length(source_revision_id) = 16)
        AND (catalogue_revision_id IS NULL OR octet_length(catalogue_revision_id) = 16)
    );

ALTER TABLE _orna_kernel.resource_audit_events
    ADD CONSTRAINT resource_audit_events_nested_invocation_presence_check CHECK (
        nested_invocation_id IS NOT NULL
        OR (
            decision_outcome = 'denied'
            AND terminal_outcome IN ('failed', 'cancelled')
        )
    );
