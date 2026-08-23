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
FROM _orna_kernel.catalogue_revisions AS revision;
