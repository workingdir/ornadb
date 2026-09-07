-- Retain lifecycle evidence for one accepted sealed invocation. The owning
-- authenticated boundary currently provides a principal identity rather than
-- a durable session identity, so owner_principal_id is the only supported
-- ownership evidence retained here.
--
-- The pinned function revision is recovered through the immutable target
-- authority selected by (catalogue_revision_id, function_id). That relation
-- also supports sealed system targets, whose opaque revision pins do not have
-- a row in the application function-revision relation.

CREATE TABLE _orna_kernel.sealed_invocation_lifecycle (
    invocation_id bytea PRIMARY KEY,
    catalogue_revision_id bytea NOT NULL,
    source_revision_id bytea NOT NULL,
    function_id bytea NOT NULL,
    owner_principal_id bytea NOT NULL,
    started_at timestamp with time zone NOT NULL DEFAULT transaction_timestamp(),
    ended_at timestamp with time zone,
    status text NOT NULL,
    diagnostic_code smallint,
    diagnostic_class smallint,
    CONSTRAINT sealed_invocation_lifecycle_identity_lengths CHECK (
        octet_length(invocation_id) = 16
        AND octet_length(catalogue_revision_id) = 16
        AND octet_length(source_revision_id) = 16
        AND octet_length(function_id) = 16
        AND octet_length(owner_principal_id) = 16
    ),
    CONSTRAINT sealed_invocation_lifecycle_status_check CHECK (
        status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled', 'orphaned')
    ),
    CONSTRAINT sealed_invocation_lifecycle_terminal_shape_check CHECK (
        (
            status IN ('queued', 'running')
            AND ended_at IS NULL
            AND diagnostic_code IS NULL
            AND diagnostic_class IS NULL
        )
        OR (
            status = 'succeeded'
            AND ended_at IS NOT NULL
            AND diagnostic_code IS NULL
            AND diagnostic_class IS NULL
        )
        OR (
            status = 'failed'
            AND ended_at IS NOT NULL
            AND diagnostic_code BETWEEN 1 AND 5
            AND diagnostic_class BETWEEN 1 AND 2
        )
        OR (
            status = 'cancelled'
            AND ended_at IS NOT NULL
            AND (
                (diagnostic_code IS NULL AND diagnostic_class IS NULL)
                OR (diagnostic_code = 4 AND diagnostic_class = 3)
            )
        )
        OR (
            status = 'orphaned'
            AND ended_at IS NOT NULL
            AND diagnostic_code IS NULL
            AND diagnostic_class IS NULL
        )
    ),
    CONSTRAINT sealed_invocation_lifecycle_terminal_time_check CHECK (
        ended_at IS NULL OR ended_at >= started_at
    ),
    CONSTRAINT sealed_invocation_lifecycle_snapshot_fk
        FOREIGN KEY (catalogue_revision_id, source_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(id, source_revision_id),
    CONSTRAINT sealed_invocation_lifecycle_target_fk
        FOREIGN KEY (catalogue_revision_id, function_id)
        REFERENCES _orna_kernel.invocation_target_authorities(
            catalogue_revision_id,
            function_id
        ),
    CONSTRAINT sealed_invocation_lifecycle_owner_fk
        FOREIGN KEY (owner_principal_id)
        REFERENCES _orna_kernel.security_principals(id)
);

CREATE INDEX sealed_invocation_lifecycle_owner_started_index
    ON _orna_kernel.sealed_invocation_lifecycle (owner_principal_id, started_at);

REVOKE ALL ON TABLE _orna_kernel.sealed_invocation_lifecycle FROM PUBLIC;
