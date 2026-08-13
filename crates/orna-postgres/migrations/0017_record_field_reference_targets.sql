ALTER TABLE _orna_kernel.definition_references
    ADD COLUMN target_record_field_catalogue_revision_id bytea NULL;

ALTER TABLE _orna_kernel.definition_references
    DROP CONSTRAINT definition_references_target_kind_check,
    DROP CONSTRAINT definition_references_target_owner_shape_check,
    DROP CONSTRAINT definition_references_reference_target_compatibility_check,
    ADD CONSTRAINT definition_references_target_kind_check CHECK (target_kind IN (
        'object_type', 'field', 'record_field', 'function', 'parameter',
        'expression', 'value_type', 'enum_type', 'record_type'
    )),
    ADD CONSTRAINT definition_references_reference_target_compatibility_check CHECK (
        (reference_kind = 'function_call' AND target_kind = 'function')
        OR (reference_kind IN ('named_type', 'object_reference', 'query_object')
            AND target_kind = 'object_type')
        OR (reference_kind = 'parameter_read' AND target_kind = 'parameter')
        OR (reference_kind = 'query_field' AND target_kind = 'field')
        OR (reference_kind = 'expression' AND target_kind = 'expression')
        OR (reference_kind = 'write_object' AND target_kind = 'object_type')
        OR (reference_kind = 'write_field' AND target_kind IN ('field', 'record_field'))
        OR (reference_kind = 'named_type'
            AND target_kind IN ('value_type', 'enum_type', 'record_type'))
    ),
    ADD CONSTRAINT definition_references_target_owner_shape_check CHECK (
        (target_kind IN ('field', 'record_field')
            AND target_owner_type_id IS NOT NULL
            AND target_owner_function_id IS NULL)
        OR (target_kind = 'parameter'
            AND target_owner_type_id IS NULL
            AND target_owner_function_id IS NOT NULL)
        OR (target_kind NOT IN ('field', 'record_field', 'parameter')
            AND target_owner_type_id IS NULL
            AND target_owner_function_id IS NULL)
    ),
    ADD CONSTRAINT definition_references_target_record_field_revision_length CHECK (
        target_record_field_catalogue_revision_id IS NULL
        OR octet_length(target_record_field_catalogue_revision_id) = 16
    ),
    ADD CONSTRAINT definition_references_target_field_revision_shape CHECK (
        (target_kind = 'field'
            AND target_record_field_catalogue_revision_id IS NULL)
        OR (target_kind = 'record_field'
            AND target_record_field_catalogue_revision_id = catalogue_revision_id)
        OR (target_kind NOT IN ('field', 'record_field')
            AND target_record_field_catalogue_revision_id IS NULL)
    ),
    ADD CONSTRAINT definition_references_record_field_target_fk
        FOREIGN KEY (
            target_record_field_catalogue_revision_id,
            target_owner_type_id,
            target_definition_id
        )
        REFERENCES _orna_kernel.catalogue_record_value_fields(
            catalogue_revision_id,
            owner_type_id,
            field_id
        )
        DEFERRABLE INITIALLY DEFERRED;

DROP INDEX _orna_kernel.definition_references_direct_target_index;

CREATE INDEX definition_references_record_field_target_index
    ON _orna_kernel.definition_references (
        target_record_field_catalogue_revision_id,
        target_owner_type_id,
        target_definition_id
    )
    WHERE target_kind = 'record_field';

CREATE INDEX definition_references_direct_target_index
    ON _orna_kernel.definition_references (
        target_kind,
        target_definition_id,
        catalogue_revision_id
    )
    WHERE target_kind NOT IN ('field', 'record_field', 'parameter');
