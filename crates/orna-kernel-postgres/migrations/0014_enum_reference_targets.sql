-- Bind named-type evidence for application enums without overloading the
-- pinned standard value target. The duplicate catalogue revision is nullable
-- so PostgreSQL can enforce the enum-only polymorphic foreign key.

ALTER TABLE _orna_kernel.definition_references
    ADD COLUMN target_enum_catalogue_revision_id bytea NULL,
    DROP CONSTRAINT definition_references_target_kind_check,
    DROP CONSTRAINT definition_references_reference_target_compatibility_check,
    ADD CONSTRAINT definition_references_target_kind_check CHECK (target_kind IN (
        'object_type',
        'field',
        'function',
        'parameter',
        'expression',
        'value_type',
        'enum_type'
    )),
    ADD CONSTRAINT definition_references_reference_target_compatibility_check CHECK (
        (reference_kind = 'function_call' AND target_kind = 'function')
        OR (
            reference_kind IN ('named_type', 'object_reference', 'query_object')
            AND target_kind = 'object_type'
        )
        OR (reference_kind = 'parameter_read' AND target_kind = 'parameter')
        OR (reference_kind = 'query_field' AND target_kind = 'field')
        OR (reference_kind = 'expression' AND target_kind = 'expression')
        OR (reference_kind = 'write_object' AND target_kind = 'object_type')
        OR (reference_kind = 'write_field' AND target_kind = 'field')
        OR (reference_kind = 'named_type' AND target_kind = 'value_type')
        OR (reference_kind = 'named_type' AND target_kind = 'enum_type')
    ),
    ADD CONSTRAINT definition_references_target_enum_revision_length CHECK (
        target_enum_catalogue_revision_id IS NULL
        OR octet_length(target_enum_catalogue_revision_id) = 16
    ),
    ADD CONSTRAINT definition_references_target_enum_revision_shape CHECK (
        (
            target_kind = 'enum_type'
            AND target_enum_catalogue_revision_id = catalogue_revision_id
        )
        OR (
            target_kind <> 'enum_type'
            AND target_enum_catalogue_revision_id IS NULL
        )
    ),
    ADD CONSTRAINT definition_references_enum_type_target_fk
        FOREIGN KEY (
            target_enum_catalogue_revision_id,
            target_definition_id
        )
        REFERENCES _orna_kernel.catalogue_enum_types(
            catalogue_revision_id,
            type_id
        )
        DEFERRABLE INITIALLY DEFERRED;

CREATE INDEX definition_references_enum_type_target_index
    ON _orna_kernel.definition_references (
        target_enum_catalogue_revision_id,
        target_definition_id
    )
    WHERE target_kind = 'enum_type';
