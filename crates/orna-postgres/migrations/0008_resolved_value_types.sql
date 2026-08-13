ALTER TABLE _orna_kernel.catalogue_fields
    ADD COLUMN value_type_id bytea NULL,
    ADD COLUMN value_standard_library_revision_id bytea NULL,
    DROP CONSTRAINT catalogue_fields_type_kind_check,
    DROP CONSTRAINT catalogue_fields_check,
    ADD CONSTRAINT catalogue_fields_type_kind_check
        CHECK (type_kind IN ('scalar', 'named', 'reference', 'value')),
    ADD CONSTRAINT catalogue_fields_check CHECK (
        (type_kind = 'scalar'
            AND scalar_type IS NOT NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL)
        OR (type_kind IN ('named', 'reference')
            AND scalar_type IS NULL
            AND target_type_id IS NOT NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL)
        OR (type_kind = 'value'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NOT NULL
            AND value_standard_library_revision_id IS NOT NULL)
    ),
    ADD CONSTRAINT cat_fields_val_type_len
        CHECK (value_type_id IS NULL OR octet_length(value_type_id) = 16),
    ADD CONSTRAINT cat_fields_val_std_rev_len CHECK (
        value_standard_library_revision_id IS NULL
        OR octet_length(value_standard_library_revision_id) = 16
    ),
    ADD CONSTRAINT cat_fields_val_pin_fk
        FOREIGN KEY (catalogue_revision_id, value_standard_library_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(
            id,
            standard_library_revision_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT cat_fields_val_type_fk
        FOREIGN KEY (value_standard_library_revision_id, value_type_id)
        REFERENCES _orna_kernel.standard_catalogue_value_types(
            standard_library_revision_id,
            type_id
        )
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE _orna_kernel.catalogue_function_parameters
    ADD COLUMN value_type_id bytea NULL,
    ADD COLUMN value_standard_library_revision_id bytea NULL,
    DROP CONSTRAINT catalogue_function_parameters_type_kind_check,
    DROP CONSTRAINT catalogue_function_parameters_check,
    ADD CONSTRAINT catalogue_function_parameters_type_kind_check
        CHECK (type_kind IN ('scalar', 'named', 'reference', 'value')),
    ADD CONSTRAINT catalogue_function_parameters_check CHECK (
        (type_kind = 'scalar'
            AND scalar_type IS NOT NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL)
        OR (type_kind IN ('named', 'reference')
            AND scalar_type IS NULL
            AND target_type_id IS NOT NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL)
        OR (type_kind = 'value'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NOT NULL
            AND value_standard_library_revision_id IS NOT NULL)
    ),
    ADD CONSTRAINT cat_fn_params_val_type_len
        CHECK (value_type_id IS NULL OR octet_length(value_type_id) = 16),
    ADD CONSTRAINT cat_fn_params_val_std_rev_len CHECK (
        value_standard_library_revision_id IS NULL
        OR octet_length(value_standard_library_revision_id) = 16
    ),
    ADD CONSTRAINT cat_fn_params_val_pin_fk
        FOREIGN KEY (catalogue_revision_id, value_standard_library_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(
            id,
            standard_library_revision_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT cat_fn_params_val_type_fk
        FOREIGN KEY (value_standard_library_revision_id, value_type_id)
        REFERENCES _orna_kernel.standard_catalogue_value_types(
            standard_library_revision_id,
            type_id
        )
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE _orna_kernel.catalogue_function_return_columns
    ADD COLUMN value_type_id bytea NULL,
    ADD COLUMN value_standard_library_revision_id bytea NULL,
    DROP CONSTRAINT catalogue_function_return_columns_type_kind_check,
    DROP CONSTRAINT catalogue_function_return_columns_check,
    ADD CONSTRAINT catalogue_function_return_columns_type_kind_check
        CHECK (type_kind IN ('scalar', 'named', 'reference', 'value')),
    ADD CONSTRAINT catalogue_function_return_columns_check CHECK (
        (type_kind = 'scalar'
            AND scalar_type IS NOT NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL)
        OR (type_kind IN ('named', 'reference')
            AND scalar_type IS NULL
            AND target_type_id IS NOT NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL)
        OR (type_kind = 'value'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NOT NULL
            AND value_standard_library_revision_id IS NOT NULL)
    ),
    ADD CONSTRAINT cat_fn_ret_cols_val_type_len
        CHECK (value_type_id IS NULL OR octet_length(value_type_id) = 16),
    ADD CONSTRAINT cat_fn_ret_cols_val_std_rev_len CHECK (
        value_standard_library_revision_id IS NULL
        OR octet_length(value_standard_library_revision_id) = 16
    ),
    ADD CONSTRAINT cat_fn_ret_cols_val_pin_fk
        FOREIGN KEY (catalogue_revision_id, value_standard_library_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(
            id,
            standard_library_revision_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT cat_fn_ret_cols_val_type_fk
        FOREIGN KEY (value_standard_library_revision_id, value_type_id)
        REFERENCES _orna_kernel.standard_catalogue_value_types(
            standard_library_revision_id,
            type_id
        )
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE _orna_kernel.catalogue_functions
    ADD COLUMN return_value_type_id bytea NULL,
    ADD COLUMN return_standard_library_revision_id bytea NULL,
    DROP CONSTRAINT catalogue_functions_return_type_kind_check,
    DROP CONSTRAINT catalogue_functions_check1,
    ADD CONSTRAINT catalogue_functions_return_type_kind_check
        CHECK (return_type_kind IN ('scalar', 'named', 'reference', 'value')),
    ADD CONSTRAINT catalogue_functions_check1 CHECK (
        (return_shape = 'rows'
            AND return_type_kind IS NULL
            AND return_scalar_type IS NULL
            AND return_target_type_id IS NULL
            AND return_value_type_id IS NULL
            AND return_standard_library_revision_id IS NULL)
        OR (return_shape = 'single' AND (
            (return_type_kind = 'scalar'
                AND return_scalar_type IS NOT NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL)
            OR (return_type_kind IN ('named', 'reference')
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NOT NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL)
            OR (return_type_kind = 'value'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NOT NULL
                AND return_standard_library_revision_id IS NOT NULL)
        ))
    ),
    ADD CONSTRAINT cat_funcs_ret_val_type_len CHECK (
        return_value_type_id IS NULL
        OR octet_length(return_value_type_id) = 16
    ),
    ADD CONSTRAINT cat_funcs_ret_val_std_rev_len CHECK (
        return_standard_library_revision_id IS NULL
        OR octet_length(return_standard_library_revision_id) = 16
    ),
    ADD CONSTRAINT cat_funcs_ret_val_pin_fk
        FOREIGN KEY (catalogue_revision_id, return_standard_library_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(
            id,
            standard_library_revision_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT cat_funcs_ret_val_type_fk
        FOREIGN KEY (return_standard_library_revision_id, return_value_type_id)
        REFERENCES _orna_kernel.standard_catalogue_value_types(
            standard_library_revision_id,
            type_id
        )
        DEFERRABLE INITIALLY DEFERRED;
