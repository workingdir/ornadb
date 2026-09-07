-- An accepted sealed invocation can be denied before a target authority is
-- resolved. Keep its lifecycle evidence without inventing a target tuple.
-- When present, the full pinned tuple remains subject to the existing
-- catalogue, source, and authority foreign keys.
ALTER TABLE _orna_kernel.sealed_invocation_lifecycle
    ALTER COLUMN catalogue_revision_id DROP NOT NULL,
    ALTER COLUMN source_revision_id DROP NOT NULL,
    ALTER COLUMN function_id DROP NOT NULL,
    ADD CONSTRAINT sealed_invocation_lifecycle_target_identity_shape_check CHECK (
        (
            catalogue_revision_id IS NULL
            AND source_revision_id IS NULL
            AND function_id IS NULL
        )
        OR (
            catalogue_revision_id IS NOT NULL
            AND source_revision_id IS NOT NULL
            AND function_id IS NOT NULL
        )
    );
