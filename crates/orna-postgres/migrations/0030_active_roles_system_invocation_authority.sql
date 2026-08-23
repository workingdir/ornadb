-- ADR 0073: add the SET-valued active-roles system identity to every
-- application catalogue revision that already has the two scalar anchors.
-- The row is an audit anchor only. It does not define or execute a catalogue
-- function.

INSERT INTO _orna_kernel.invocation_target_authorities (
    catalogue_revision_id,
    function_id,
    target_class,
    function_revision_id,
    standard_library_revision_id
)
SELECT
    revision.id,
    decode('00000000000000000000000000000042', 'hex'),
    'system',
    decode('00000000000000000000000000000042', 'hex'),
    NULL
FROM _orna_kernel.catalogue_revisions AS revision
WHERE NOT EXISTS (
    SELECT 1
    FROM _orna_kernel.invocation_target_authorities AS authority
    WHERE authority.catalogue_revision_id = revision.id
      AND authority.function_id = decode('00000000000000000000000000000042', 'hex')
);

DO $$
DECLARE
    missing bigint;
BEGIN
    SELECT count(*) INTO missing
    FROM _orna_kernel.catalogue_revisions AS revision
    WHERE NOT EXISTS (
        SELECT 1
        FROM _orna_kernel.invocation_target_authorities AS authority
        WHERE authority.catalogue_revision_id = revision.id
          AND authority.function_id = decode('00000000000000000000000000000042', 'hex')
          AND authority.target_class = 'system'
          AND authority.function_revision_id = decode('00000000000000000000000000000042', 'hex')
          AND authority.standard_library_revision_id IS NULL
    );
    IF missing <> 0 THEN
        RAISE EXCEPTION
            'active-roles system invocation authority backfill is incomplete';
    END IF;
END
$$;
