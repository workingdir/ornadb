-- Resolved sealed invocations retain the authoritative CWD capture observed
-- at protected admission. Unresolved target denials remain private lifecycle
-- evidence and therefore have no public observation coordinates.
ALTER TABLE _orna_kernel.sealed_invocation_lifecycle
    ADD COLUMN admission_snapshot bytea,
    ADD COLUMN admission_generation_digest bytea,
    ADD COLUMN admission_runtime_id bytea,
    ADD COLUMN admission_runtime_generation bigint,
    ADD CONSTRAINT sealed_invocation_lifecycle_admission_capture_shape_check CHECK (
        (
            catalogue_revision_id IS NULL
            AND source_revision_id IS NULL
            AND function_id IS NULL
            AND admission_snapshot IS NULL
            AND admission_generation_digest IS NULL
            AND admission_runtime_id IS NULL
            AND admission_runtime_generation IS NULL
        )
        OR (
            catalogue_revision_id IS NOT NULL
            AND source_revision_id IS NOT NULL
            AND function_id IS NOT NULL
            AND admission_snapshot IS NOT NULL
            AND admission_generation_digest IS NOT NULL
            AND admission_runtime_id IS NOT NULL
            AND admission_runtime_generation IS NOT NULL
            AND octet_length(admission_snapshot) > 0
            AND octet_length(admission_generation_digest) = 32
            AND octet_length(admission_runtime_id) = 16
            AND admission_runtime_generation >= 0
        )
    ) NOT VALID;
