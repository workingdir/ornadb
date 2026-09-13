-- Add the read-only identity projection required by catalogue references.
-- Its rows are sourced from the authoritative catalogue tables; no semantic
-- object IDs are allocated here. The physical relation identity is the
-- foundation SYS_OBJECT_TABLE_ID compatibility coordinate.
CREATE VIEW _orna_kernel.catalogue_objects (
    catalogue_revision_id,
    object_id,
    object_kind,
    type_form
) AS
SELECT catalogue_revision_id, function_id AS object_id, 'function'::text AS object_kind,
       NULL::text AS type_form
  FROM _orna_kernel.catalogue_functions
UNION
SELECT catalogue_revision_id, type_id AS object_id, 'type'::text AS object_kind,
       'named'::text AS type_form
  FROM _orna_kernel.catalogue_object_types
UNION
SELECT catalogue_revision_id, type_id AS object_id, 'type'::text AS object_kind,
       'named'::text AS type_form
  FROM _orna_kernel.catalogue_enum_types
UNION
SELECT catalogue_revision_id, type_id AS object_id, 'type'::text AS object_kind,
       'named'::text AS type_form
  FROM _orna_kernel.catalogue_record_value_types
UNION
SELECT revision.id AS catalogue_revision_id, function.function_id AS object_id,
       'function'::text AS object_kind, NULL::text AS type_form
  FROM _orna_kernel.catalogue_revisions AS revision
  JOIN _orna_kernel.standard_catalogue_functions AS function
    ON function.standard_library_revision_id = revision.standard_library_revision_id
UNION
SELECT revision.id AS catalogue_revision_id, type.type_id AS object_id,
       'type'::text AS object_kind, 'value'::text AS type_form
  FROM _orna_kernel.catalogue_revisions AS revision
  JOIN _orna_kernel.standard_catalogue_value_types AS type
    ON type.standard_library_revision_id = revision.standard_library_revision_id
UNION
SELECT revision.id AS catalogue_revision_id, type.type_id AS object_id,
       'type'::text AS object_kind, 'named'::text AS type_form
  FROM _orna_kernel.catalogue_revisions AS revision
  JOIN _orna_kernel.standard_catalogue_enum_types AS type
    ON type.standard_library_revision_id = revision.standard_library_revision_id;

REVOKE ALL ON TABLE _orna_kernel.catalogue_objects FROM PUBLIC;

-- Retain only witnesses produced after the trusted sealed-admission lookup.
-- Nullable columns preserve older admissions and rows whose exact sys.Type
-- projection is not available; such rows are not presented as complete sys
-- Invocation rows by the bounded observation DTO.
ALTER TABLE _orna_kernel.sealed_invocation_lifecycle
    ADD COLUMN function_reference bytea,
    ADD COLUMN result_type_reference bytea,
    ADD CONSTRAINT sealed_invocation_lifecycle_function_reference_shape_check
        CHECK (
            function_reference IS NULL
            OR (
                function_id IS NOT NULL
                AND octet_length(function_reference) > 0
            )
        ),
    ADD CONSTRAINT sealed_invocation_lifecycle_result_type_reference_shape_check
        CHECK (
            result_type_reference IS NULL
            OR (
                function_id IS NOT NULL
                AND octet_length(result_type_reference) > 0
            )
        );

ALTER TABLE _orna_kernel.sealed_invocation_argument_metadata
    ADD COLUMN type_reference bytea,
    ADD CONSTRAINT sealed_invocation_argument_metadata_type_reference_shape_check
        CHECK (
            type_reference IS NULL
            OR (
                target_type_id IS NOT NULL
                AND octet_length(type_reference) > 0
            )
        );
