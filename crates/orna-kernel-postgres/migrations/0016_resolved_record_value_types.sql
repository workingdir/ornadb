-- Store record uses separately from object, standard value, and enum
-- identities. Every foreign key names the exact active record definition.

ALTER TABLE _orna_kernel.catalogue_fields
    ADD COLUMN record_type_id bytea NULL,
    DROP CONSTRAINT catalogue_fields_type_kind_check,
    DROP CONSTRAINT catalogue_fields_check,
    ADD CONSTRAINT catalogue_fields_type_kind_check
        CHECK (type_kind IN ('scalar', 'named', 'reference', 'value', 'enum', 'record')),
    ADD CONSTRAINT catalogue_fields_check CHECK (
        (type_kind = 'scalar' AND scalar_type IS NOT NULL
            AND target_type_id IS NULL AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL AND record_type_id IS NULL)
        OR (type_kind IN ('named', 'reference') AND scalar_type IS NULL
            AND target_type_id IS NOT NULL AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL AND record_type_id IS NULL)
        OR (type_kind = 'value' AND scalar_type IS NULL
            AND target_type_id IS NULL AND value_type_id IS NOT NULL
            AND value_standard_library_revision_id IS NOT NULL
            AND enum_type_id IS NULL AND record_type_id IS NULL)
        OR (type_kind = 'enum' AND scalar_type IS NULL
            AND target_type_id IS NULL AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NOT NULL AND record_type_id IS NULL)
        OR (type_kind = 'record' AND scalar_type IS NULL
            AND target_type_id IS NULL AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL AND record_type_id IS NOT NULL)
    ),
    ADD CONSTRAINT cat_fields_record_type_len
        CHECK (record_type_id IS NULL OR octet_length(record_type_id) = 16),
    ADD CONSTRAINT cat_fields_record_type_fk
        FOREIGN KEY (catalogue_revision_id, record_type_id)
        REFERENCES _orna_kernel.catalogue_record_value_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE _orna_kernel.catalogue_function_parameters
    ADD COLUMN record_type_id bytea NULL,
    DROP CONSTRAINT catalogue_function_parameters_type_kind_check,
    DROP CONSTRAINT catalogue_function_parameters_check,
    ADD CONSTRAINT catalogue_function_parameters_type_kind_check
        CHECK (type_kind IN ('scalar', 'named', 'reference', 'value', 'enum', 'record')),
    ADD CONSTRAINT catalogue_function_parameters_check CHECK (
        (type_kind = 'scalar' AND scalar_type IS NOT NULL
            AND target_type_id IS NULL AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL AND record_type_id IS NULL)
        OR (type_kind IN ('named', 'reference') AND scalar_type IS NULL
            AND target_type_id IS NOT NULL AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL AND record_type_id IS NULL)
        OR (type_kind = 'value' AND scalar_type IS NULL
            AND target_type_id IS NULL AND value_type_id IS NOT NULL
            AND value_standard_library_revision_id IS NOT NULL
            AND enum_type_id IS NULL AND record_type_id IS NULL)
        OR (type_kind = 'enum' AND scalar_type IS NULL
            AND target_type_id IS NULL AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NOT NULL AND record_type_id IS NULL)
        OR (type_kind = 'record' AND scalar_type IS NULL
            AND target_type_id IS NULL AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL AND record_type_id IS NOT NULL)
    ),
    ADD CONSTRAINT cat_fn_params_record_type_len
        CHECK (record_type_id IS NULL OR octet_length(record_type_id) = 16),
    ADD CONSTRAINT cat_fn_params_record_type_fk
        FOREIGN KEY (catalogue_revision_id, record_type_id)
        REFERENCES _orna_kernel.catalogue_record_value_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE _orna_kernel.catalogue_function_return_columns
    ADD COLUMN record_type_id bytea NULL,
    DROP CONSTRAINT catalogue_function_return_columns_type_kind_check,
    DROP CONSTRAINT catalogue_function_return_columns_check,
    ADD CONSTRAINT catalogue_function_return_columns_type_kind_check
        CHECK (type_kind IN ('scalar', 'named', 'reference', 'value', 'enum', 'record')),
    ADD CONSTRAINT catalogue_function_return_columns_check CHECK (
        (type_kind = 'scalar' AND scalar_type IS NOT NULL
            AND target_type_id IS NULL AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL AND record_type_id IS NULL)
        OR (type_kind IN ('named', 'reference') AND scalar_type IS NULL
            AND target_type_id IS NOT NULL AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL AND record_type_id IS NULL)
        OR (type_kind = 'value' AND scalar_type IS NULL
            AND target_type_id IS NULL AND value_type_id IS NOT NULL
            AND value_standard_library_revision_id IS NOT NULL
            AND enum_type_id IS NULL AND record_type_id IS NULL)
        OR (type_kind = 'enum' AND scalar_type IS NULL
            AND target_type_id IS NULL AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NOT NULL AND record_type_id IS NULL)
        OR (type_kind = 'record' AND scalar_type IS NULL
            AND target_type_id IS NULL AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL AND record_type_id IS NOT NULL)
    ),
    ADD CONSTRAINT cat_fn_ret_cols_record_type_len
        CHECK (record_type_id IS NULL OR octet_length(record_type_id) = 16),
    ADD CONSTRAINT cat_fn_ret_cols_record_type_fk
        FOREIGN KEY (catalogue_revision_id, record_type_id)
        REFERENCES _orna_kernel.catalogue_record_value_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE _orna_kernel.catalogue_functions
    ADD COLUMN return_record_type_id bytea NULL,
    DROP CONSTRAINT catalogue_functions_return_type_kind_check,
    DROP CONSTRAINT catalogue_functions_check1,
    ADD CONSTRAINT catalogue_functions_return_type_kind_check
        CHECK (return_type_kind IN ('scalar', 'named', 'reference', 'value', 'enum', 'record')),
    ADD CONSTRAINT catalogue_functions_check1 CHECK (
        (return_shape = 'rows' AND return_type_kind IS NULL
            AND return_scalar_type IS NULL AND return_target_type_id IS NULL
            AND return_value_type_id IS NULL
            AND return_standard_library_revision_id IS NULL
            AND return_enum_type_id IS NULL AND return_record_type_id IS NULL)
        OR (return_shape = 'single' AND (
            (return_type_kind = 'scalar' AND return_scalar_type IS NOT NULL
                AND return_target_type_id IS NULL AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL AND return_record_type_id IS NULL)
            OR (return_type_kind IN ('named', 'reference') AND return_scalar_type IS NULL
                AND return_target_type_id IS NOT NULL AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL AND return_record_type_id IS NULL)
            OR (return_type_kind = 'value' AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL AND return_value_type_id IS NOT NULL
                AND return_standard_library_revision_id IS NOT NULL
                AND return_enum_type_id IS NULL AND return_record_type_id IS NULL)
            OR (return_type_kind = 'enum' AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NOT NULL AND return_record_type_id IS NULL)
            OR (return_type_kind = 'record' AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL AND return_record_type_id IS NOT NULL)
        ))
    ),
    ADD CONSTRAINT cat_funcs_ret_record_type_len CHECK (
        return_record_type_id IS NULL OR octet_length(return_record_type_id) = 16
    ),
    ADD CONSTRAINT cat_funcs_ret_record_type_fk
        FOREIGN KEY (catalogue_revision_id, return_record_type_id)
        REFERENCES _orna_kernel.catalogue_record_value_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE _orna_kernel.definition_references
    ADD COLUMN target_record_catalogue_revision_id bytea NULL,
    DROP CONSTRAINT definition_references_target_kind_check,
    DROP CONSTRAINT definition_references_reference_target_compatibility_check,
    ADD CONSTRAINT definition_references_target_kind_check CHECK (target_kind IN (
        'object_type', 'field', 'function', 'parameter', 'expression',
        'value_type', 'enum_type', 'record_type'
    )),
    ADD CONSTRAINT definition_references_reference_target_compatibility_check CHECK (
        (reference_kind = 'function_call' AND target_kind = 'function')
        OR (reference_kind IN ('named_type', 'object_reference', 'query_object')
            AND target_kind = 'object_type')
        OR (reference_kind = 'parameter_read' AND target_kind = 'parameter')
        OR (reference_kind = 'query_field' AND target_kind = 'field')
        OR (reference_kind = 'expression' AND target_kind = 'expression')
        OR (reference_kind = 'write_object' AND target_kind = 'object_type')
        OR (reference_kind = 'write_field' AND target_kind = 'field')
        OR (reference_kind = 'named_type' AND target_kind IN ('value_type', 'enum_type', 'record_type'))
    ),
    ADD CONSTRAINT definition_references_target_record_revision_length CHECK (
        target_record_catalogue_revision_id IS NULL
        OR octet_length(target_record_catalogue_revision_id) = 16
    ),
    ADD CONSTRAINT definition_references_target_record_revision_shape CHECK (
        (target_kind = 'record_type'
            AND target_record_catalogue_revision_id = catalogue_revision_id)
        OR (target_kind <> 'record_type'
            AND target_record_catalogue_revision_id IS NULL)
    ),
    ADD CONSTRAINT definition_references_record_type_target_fk
        FOREIGN KEY (target_record_catalogue_revision_id, target_definition_id)
        REFERENCES _orna_kernel.catalogue_record_value_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED;

CREATE INDEX definition_references_record_type_target_index
    ON _orna_kernel.definition_references (
        target_record_catalogue_revision_id,
        target_definition_id
    )
    WHERE target_kind = 'record_type';
