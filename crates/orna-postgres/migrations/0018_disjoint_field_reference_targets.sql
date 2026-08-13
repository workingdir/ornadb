ALTER TABLE _orna_kernel.definition_references
    ADD COLUMN target_record_field_owner_type_id bytea NULL,
    DROP CONSTRAINT definition_references_target_owner_shape_check,
    DROP CONSTRAINT definition_references_record_field_target_fk;

UPDATE _orna_kernel.definition_references
SET target_record_field_owner_type_id = target_owner_type_id,
    target_owner_type_id = NULL
WHERE target_kind = 'record_field';

ALTER TABLE _orna_kernel.definition_references
    ADD CONSTRAINT definition_references_target_record_field_owner_type_id_check CHECK (
        target_record_field_owner_type_id IS NULL
        OR octet_length(target_record_field_owner_type_id) = 16
    ),
    ADD CONSTRAINT definition_references_target_owner_shape_check CHECK (
        (target_kind = 'field'
            AND target_owner_type_id IS NOT NULL
            AND target_record_field_owner_type_id IS NULL
            AND target_owner_function_id IS NULL)
        OR (target_kind = 'record_field'
            AND target_owner_type_id IS NULL
            AND target_record_field_owner_type_id IS NOT NULL
            AND target_owner_function_id IS NULL)
        OR (target_kind = 'parameter'
            AND target_owner_type_id IS NULL
            AND target_record_field_owner_type_id IS NULL
            AND target_owner_function_id IS NOT NULL)
        OR (target_kind NOT IN ('field', 'record_field', 'parameter')
            AND target_owner_type_id IS NULL
            AND target_record_field_owner_type_id IS NULL
            AND target_owner_function_id IS NULL)
    ),
    ADD CONSTRAINT definition_references_record_field_target_fk
        FOREIGN KEY (
            target_record_field_catalogue_revision_id,
            target_record_field_owner_type_id,
            target_definition_id
        )
        REFERENCES _orna_kernel.catalogue_record_value_fields(
            catalogue_revision_id,
            owner_type_id,
            field_id
        )
        DEFERRABLE INITIALLY DEFERRED;

DROP INDEX _orna_kernel.definition_references_record_field_target_index;

CREATE INDEX definition_references_record_field_target_index
    ON _orna_kernel.definition_references (
        target_record_field_catalogue_revision_id,
        target_record_field_owner_type_id,
        target_definition_id
    )
    WHERE target_kind = 'record_field';
