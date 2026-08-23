-- ADR 0072: sealed security-identity invocations use the existing
-- invocation-audit relation. These authority rows are audit anchors only;
-- they are not catalogue definitions or executable revisions. The sealed
-- function identity is repeated as the opaque revision pin because the
-- in-process system registry has no durable executable revision.

ALTER TABLE _orna_kernel.invocation_target_authorities
    DROP CONSTRAINT invocation_target_authorities_target_class_check,
    DROP CONSTRAINT invocation_target_authorities_class_shape_check;

ALTER TABLE _orna_kernel.invocation_target_authorities
    ADD CONSTRAINT invocation_target_authorities_target_class_check
        CHECK (target_class IN ('application', 'standard', 'system')),
    ADD CONSTRAINT invocation_target_authorities_class_shape_check CHECK (
        (
            target_class = 'application'
            AND standard_library_revision_id IS NULL
        )
        OR (
            target_class = 'standard'
            AND standard_library_revision_id IS NOT NULL
        )
        OR (
            target_class = 'system'
            AND standard_library_revision_id IS NULL
            AND function_revision_id = function_id
        )
    );

-- The first two admitted sealed system identities are callable through
-- sys.invoke. One audit anchor is required for each application catalogue
-- revision so the existing invocation-audit target foreign key remains
-- complete without turning the identities into catalogue functions.
INSERT INTO _orna_kernel.invocation_target_authorities (
    catalogue_revision_id,
    function_id,
    target_class,
    function_revision_id,
    standard_library_revision_id
)
SELECT
    revision.id,
    identity.function_id,
    'system',
    identity.function_id,
    NULL
FROM _orna_kernel.catalogue_revisions AS revision
CROSS JOIN (
    VALUES
        (decode('00000000000000000000000000000040', 'hex')),
        (decode('00000000000000000000000000000041', 'hex'))
) AS identity(function_id);

DO $$
DECLARE
    missing bigint;
BEGIN
    SELECT count(*) INTO missing
    FROM _orna_kernel.catalogue_revisions AS revision
    CROSS JOIN (
        VALUES
            (decode('00000000000000000000000000000040', 'hex')),
            (decode('00000000000000000000000000000041', 'hex'))
    ) AS identity(function_id)
    WHERE NOT EXISTS (
        SELECT 1
        FROM _orna_kernel.invocation_target_authorities AS authority
        WHERE authority.catalogue_revision_id = revision.id
          AND authority.function_id = identity.function_id
          AND authority.target_class = 'system'
          AND authority.function_revision_id = identity.function_id
          AND authority.standard_library_revision_id IS NULL
    );
    IF missing <> 0 THEN
        RAISE EXCEPTION
            'sealed system invocation authority backfill is incomplete';
    END IF;
END
$$;
