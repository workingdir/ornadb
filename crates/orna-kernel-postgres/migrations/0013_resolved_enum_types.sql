-- Store application enum uses separately from object identities and pinned
-- standard value identities. The semantic type remains catalogue-owned; this
-- tuple only preserves its stable TypeId at every application type position.

ALTER TABLE _orna_kernel.catalogue_fields
    ADD COLUMN enum_type_id bytea NULL,
    DROP CONSTRAINT catalogue_fields_type_kind_check,
    DROP CONSTRAINT catalogue_fields_check,
    ADD CONSTRAINT catalogue_fields_type_kind_check
        CHECK (type_kind IN ('scalar', 'named', 'reference', 'value', 'enum')),
    ADD CONSTRAINT catalogue_fields_check CHECK (
        (type_kind = 'scalar'
            AND scalar_type IS NOT NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL)
        OR (type_kind IN ('named', 'reference')
            AND scalar_type IS NULL
            AND target_type_id IS NOT NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL)
        OR (type_kind = 'value'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NOT NULL
            AND value_standard_library_revision_id IS NOT NULL
            AND enum_type_id IS NULL)
        OR (type_kind = 'enum'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NOT NULL)
    ),
    ADD CONSTRAINT cat_fields_enum_type_len
        CHECK (enum_type_id IS NULL OR octet_length(enum_type_id) = 16),
    ADD CONSTRAINT cat_fields_enum_type_fk
        FOREIGN KEY (catalogue_revision_id, enum_type_id)
        REFERENCES _orna_kernel.catalogue_enum_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE _orna_kernel.catalogue_function_parameters
    ADD COLUMN enum_type_id bytea NULL,
    DROP CONSTRAINT catalogue_function_parameters_type_kind_check,
    DROP CONSTRAINT catalogue_function_parameters_check,
    ADD CONSTRAINT catalogue_function_parameters_type_kind_check
        CHECK (type_kind IN ('scalar', 'named', 'reference', 'value', 'enum')),
    ADD CONSTRAINT catalogue_function_parameters_check CHECK (
        (type_kind = 'scalar'
            AND scalar_type IS NOT NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL)
        OR (type_kind IN ('named', 'reference')
            AND scalar_type IS NULL
            AND target_type_id IS NOT NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL)
        OR (type_kind = 'value'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NOT NULL
            AND value_standard_library_revision_id IS NOT NULL
            AND enum_type_id IS NULL)
        OR (type_kind = 'enum'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NOT NULL)
    ),
    ADD CONSTRAINT cat_fn_params_enum_type_len
        CHECK (enum_type_id IS NULL OR octet_length(enum_type_id) = 16),
    ADD CONSTRAINT cat_fn_params_enum_type_fk
        FOREIGN KEY (catalogue_revision_id, enum_type_id)
        REFERENCES _orna_kernel.catalogue_enum_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE _orna_kernel.catalogue_function_return_columns
    ADD COLUMN enum_type_id bytea NULL,
    DROP CONSTRAINT catalogue_function_return_columns_type_kind_check,
    DROP CONSTRAINT catalogue_function_return_columns_check,
    ADD CONSTRAINT catalogue_function_return_columns_type_kind_check
        CHECK (type_kind IN ('scalar', 'named', 'reference', 'value', 'enum')),
    ADD CONSTRAINT catalogue_function_return_columns_check CHECK (
        (type_kind = 'scalar'
            AND scalar_type IS NOT NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL)
        OR (type_kind IN ('named', 'reference')
            AND scalar_type IS NULL
            AND target_type_id IS NOT NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL)
        OR (type_kind = 'value'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NOT NULL
            AND value_standard_library_revision_id IS NOT NULL
            AND enum_type_id IS NULL)
        OR (type_kind = 'enum'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NOT NULL)
    ),
    ADD CONSTRAINT cat_fn_ret_cols_enum_type_len
        CHECK (enum_type_id IS NULL OR octet_length(enum_type_id) = 16),
    ADD CONSTRAINT cat_fn_ret_cols_enum_type_fk
        FOREIGN KEY (catalogue_revision_id, enum_type_id)
        REFERENCES _orna_kernel.catalogue_enum_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE _orna_kernel.catalogue_functions
    ADD COLUMN return_enum_type_id bytea NULL,
    DROP CONSTRAINT catalogue_functions_return_type_kind_check,
    DROP CONSTRAINT catalogue_functions_check1,
    ADD CONSTRAINT catalogue_functions_return_type_kind_check
        CHECK (return_type_kind IN ('scalar', 'named', 'reference', 'value', 'enum')),
    ADD CONSTRAINT catalogue_functions_check1 CHECK (
        (return_shape = 'rows'
            AND return_type_kind IS NULL
            AND return_scalar_type IS NULL
            AND return_target_type_id IS NULL
            AND return_value_type_id IS NULL
            AND return_standard_library_revision_id IS NULL
            AND return_enum_type_id IS NULL)
        OR (return_shape = 'single' AND (
            (return_type_kind = 'scalar'
                AND return_scalar_type IS NOT NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL)
            OR (return_type_kind IN ('named', 'reference')
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NOT NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL)
            OR (return_type_kind = 'value'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NOT NULL
                AND return_standard_library_revision_id IS NOT NULL
                AND return_enum_type_id IS NULL)
            OR (return_type_kind = 'enum'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NOT NULL)
        ))
    ),
    ADD CONSTRAINT cat_funcs_ret_enum_type_len CHECK (
        return_enum_type_id IS NULL
        OR octet_length(return_enum_type_id) = 16
    ),
    ADD CONSTRAINT cat_funcs_ret_enum_type_fk
        FOREIGN KEY (catalogue_revision_id, return_enum_type_id)
        REFERENCES _orna_kernel.catalogue_enum_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED;
