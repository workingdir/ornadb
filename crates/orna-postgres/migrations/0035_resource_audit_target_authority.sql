-- ADR 0078: resource audit targets include verified-standard functions.
ALTER TABLE _orna_kernel.resource_audit_events
    DROP CONSTRAINT resource_audit_events_target_fk;

ALTER TABLE _orna_kernel.resource_audit_events
    ADD CONSTRAINT resource_audit_events_target_fk
    FOREIGN KEY (catalogue_revision_id, target_function_id)
    REFERENCES _orna_kernel.invocation_target_authorities(
        catalogue_revision_id,
        function_id
    );
