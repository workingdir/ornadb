-- Field and parameter identities are stable only within their owning
-- object type or function. Preserve the old catalogue-wide keys until every
-- legacy reference has one exact owner, then enforce the qualified keys.

ALTER TABLE _orna_kernel.definition_references
    ADD COLUMN target_owner_type_id bytea
        CHECK (
            target_owner_type_id IS NULL
            OR octet_length(target_owner_type_id) = 16
        ),
    ADD COLUMN target_owner_function_id bytea
        CHECK (
            target_owner_function_id IS NULL
            OR octet_length(target_owner_function_id) = 16
        );

UPDATE _orna_kernel.definition_references AS reference
SET target_owner_type_id = (
    SELECT field.owner_type_id
    FROM _orna_kernel.catalogue_fields AS field
    WHERE field.catalogue_revision_id = reference.catalogue_revision_id
      AND field.field_id = reference.target_definition_id
)
WHERE reference.target_kind = 'field';

UPDATE _orna_kernel.definition_references AS reference
SET target_owner_function_id = (
    SELECT parameter.function_id
    FROM _orna_kernel.catalogue_function_parameters AS parameter
    WHERE parameter.catalogue_revision_id = reference.catalogue_revision_id
      AND parameter.parameter_id = reference.target_definition_id
)
WHERE reference.target_kind = 'parameter';

ALTER TABLE _orna_kernel.catalogue_fields
    DROP CONSTRAINT catalogue_fields_pkey,
    ADD CONSTRAINT catalogue_fields_pkey
        PRIMARY KEY (catalogue_revision_id, owner_type_id, field_id);

ALTER TABLE _orna_kernel.catalogue_function_parameters
    DROP CONSTRAINT catalogue_function_parameters_pkey,
    ADD CONSTRAINT catalogue_function_parameters_pkey
        PRIMARY KEY (catalogue_revision_id, function_id, parameter_id);

ALTER TABLE _orna_kernel.definition_references
    ADD CONSTRAINT definition_references_target_owner_shape_check CHECK (
        (
            target_kind = 'field'
            AND target_owner_type_id IS NOT NULL
            AND target_owner_function_id IS NULL
        )
        OR (
            target_kind = 'parameter'
            AND target_owner_type_id IS NULL
            AND target_owner_function_id IS NOT NULL
        )
        OR (
            target_kind NOT IN ('field', 'parameter')
            AND target_owner_type_id IS NULL
            AND target_owner_function_id IS NULL
        )
    ),
    ADD CONSTRAINT definition_references_reference_target_compatibility_check CHECK (
        (reference_kind = 'function_call' AND target_kind = 'function')
        OR (
            reference_kind IN ('named_type', 'object_reference', 'query_object')
            AND target_kind = 'object_type'
        )
        OR (reference_kind = 'parameter_read' AND target_kind = 'parameter')
        OR (reference_kind = 'query_field' AND target_kind = 'field')
        OR (reference_kind = 'expression' AND target_kind = 'expression')
    ),
    ADD CONSTRAINT definition_references_field_target_fk
        FOREIGN KEY (
            catalogue_revision_id,
            target_owner_type_id,
            target_definition_id
        )
        REFERENCES _orna_kernel.catalogue_fields(
            catalogue_revision_id,
            owner_type_id,
            field_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT definition_references_parameter_target_fk
        FOREIGN KEY (
            catalogue_revision_id,
            target_owner_function_id,
            target_definition_id
        )
        REFERENCES _orna_kernel.catalogue_function_parameters(
            catalogue_revision_id,
            function_id,
            parameter_id
        )
        DEFERRABLE INITIALLY DEFERRED;

DROP INDEX _orna_kernel.definition_references_target_index;

CREATE INDEX definition_references_field_target_index
    ON _orna_kernel.definition_references (
        target_owner_type_id,
        target_definition_id,
        catalogue_revision_id
    )
    WHERE target_kind = 'field';

CREATE INDEX definition_references_parameter_target_index
    ON _orna_kernel.definition_references (
        target_owner_function_id,
        target_definition_id,
        catalogue_revision_id
    )
    WHERE target_kind = 'parameter';

CREATE INDEX definition_references_direct_target_index
    ON _orna_kernel.definition_references (
        target_kind,
        target_definition_id,
        catalogue_revision_id
    )
    WHERE target_kind NOT IN ('field', 'parameter');
