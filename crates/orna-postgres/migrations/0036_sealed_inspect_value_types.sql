-- Admit the transient sealed Inspector carriers only in function parameter and
-- function result value slots. They have no standard-library revision pin and
-- must never become durable catalogue fields or ROWS return columns.

ALTER TABLE _orna_kernel.catalogue_function_parameters
    DROP CONSTRAINT catalogue_function_parameters_check,
    ADD CONSTRAINT catalogue_function_parameters_check CHECK (
        (type_kind = 'scalar'
            AND scalar_type IS NOT NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL
            AND record_type_id IS NULL)
        OR (type_kind IN ('named', 'reference')
            AND scalar_type IS NULL
            AND target_type_id IS NOT NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL
            AND record_type_id IS NULL)
        OR (type_kind = 'value'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NOT NULL
            AND (
                (value_standard_library_revision_id IS NOT NULL
                 AND value_type_id NOT IN (
                    decode('000000000000000000000000000000f3', 'hex'),
                    decode('000000000000000000000000000000f4', 'hex'),
                    decode('000000000000000000000000000000f5', 'hex'),
                    decode('000000000000000000000000000000f6', 'hex'),
                    decode('000000000000000000000000000000f8', 'hex'),
                    decode('000000000000000000000000000000f9', 'hex'),
                    decode('000000000000000000000000000000fa', 'hex'),
                    decode('000000000000000000000000000000fb', 'hex'),
                    decode('000000000000000000000000000000fc', 'hex'),
                    decode('000000000000000000000000000000fd', 'hex'),
                    decode('000000000000000000000000000000fe', 'hex'),
                    decode('000000000000000000000000000000ff', 'hex')
                )
                )
                OR (value_standard_library_revision_id IS NULL
                 AND value_type_id IN (
                    decode('000000000000000000000000000000f3', 'hex'),
                    decode('000000000000000000000000000000f4', 'hex'),
                    decode('000000000000000000000000000000f5', 'hex'),
                    decode('000000000000000000000000000000f6', 'hex'),
                    decode('000000000000000000000000000000f8', 'hex'),
                    decode('000000000000000000000000000000f9', 'hex'),
                    decode('000000000000000000000000000000fa', 'hex'),
                    decode('000000000000000000000000000000fb', 'hex'),
                    decode('000000000000000000000000000000fc', 'hex'),
                    decode('000000000000000000000000000000fd', 'hex'),
                    decode('000000000000000000000000000000fe', 'hex'),
                    decode('000000000000000000000000000000ff', 'hex')
                )
                )
            )
            AND enum_type_id IS NULL
            AND record_type_id IS NULL)
        OR (type_kind = 'enum'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NOT NULL
            AND record_type_id IS NULL)
        OR (type_kind = 'record'
            AND scalar_type IS NULL
            AND target_type_id IS NULL
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL
            AND record_type_id IS NOT NULL)
    ),
    ADD CONSTRAINT catalogue_function_parameters_value_pin_check CHECK (
        type_kind <> 'value'
        OR (
            value_standard_library_revision_id IS NOT NULL
            AND value_type_id IS NOT NULL
            AND value_type_id NOT IN (
                decode('000000000000000000000000000000f3', 'hex'),
                decode('000000000000000000000000000000f4', 'hex'),
                decode('000000000000000000000000000000f5', 'hex'),
                decode('000000000000000000000000000000f6', 'hex'),
                decode('000000000000000000000000000000f8', 'hex'),
                decode('000000000000000000000000000000f9', 'hex'),
                decode('000000000000000000000000000000fa', 'hex'),
                decode('000000000000000000000000000000fb', 'hex'),
                decode('000000000000000000000000000000fc', 'hex'),
                decode('000000000000000000000000000000fd', 'hex'),
                decode('000000000000000000000000000000fe', 'hex'),
                decode('000000000000000000000000000000ff', 'hex')
            )
        )
        OR (value_standard_library_revision_id IS NULL
            AND value_type_id IS NOT NULL
            AND value_type_id IN (
                decode('000000000000000000000000000000f3', 'hex'),
                decode('000000000000000000000000000000f4', 'hex'),
                decode('000000000000000000000000000000f5', 'hex'),
                decode('000000000000000000000000000000f6', 'hex'),
                decode('000000000000000000000000000000f8', 'hex'),
                decode('000000000000000000000000000000f9', 'hex'),
                decode('000000000000000000000000000000fa', 'hex'),
                decode('000000000000000000000000000000fb', 'hex'),
                decode('000000000000000000000000000000fc', 'hex'),
                decode('000000000000000000000000000000fd', 'hex'),
                decode('000000000000000000000000000000fe', 'hex'),
                decode('000000000000000000000000000000ff', 'hex')
            )
        )
    );

ALTER TABLE _orna_kernel.catalogue_functions
    DROP CONSTRAINT catalogue_functions_check1,
    ADD CONSTRAINT catalogue_functions_check1 CHECK (
        (return_shape = 'rows'
            AND return_type_kind IS NULL
            AND return_scalar_type IS NULL
            AND return_target_type_id IS NULL
            AND return_value_type_id IS NULL
            AND return_standard_library_revision_id IS NULL
            AND return_enum_type_id IS NULL
            AND return_record_type_id IS NULL)
        OR (return_shape = 'single' AND (
            (return_type_kind = 'scalar'
                AND return_scalar_type IS NOT NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind IN ('named', 'reference')
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NOT NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind = 'value'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NOT NULL
                AND (
                    (return_standard_library_revision_id IS NOT NULL
                     AND return_value_type_id NOT IN (
                        decode('000000000000000000000000000000f3', 'hex'),
                        decode('000000000000000000000000000000f4', 'hex'),
                        decode('000000000000000000000000000000f5', 'hex'),
                        decode('000000000000000000000000000000f6', 'hex'),
                        decode('000000000000000000000000000000f8', 'hex'),
                        decode('000000000000000000000000000000f9', 'hex'),
                        decode('000000000000000000000000000000fa', 'hex'),
                        decode('000000000000000000000000000000fb', 'hex'),
                        decode('000000000000000000000000000000fc', 'hex'),
                        decode('000000000000000000000000000000fd', 'hex'),
                        decode('000000000000000000000000000000fe', 'hex'),
                        decode('000000000000000000000000000000ff', 'hex')
                    )
                    )
                    OR (return_standard_library_revision_id IS NULL
                     AND return_value_type_id IN (
                        decode('000000000000000000000000000000f3', 'hex'),
                        decode('000000000000000000000000000000f4', 'hex'),
                        decode('000000000000000000000000000000f5', 'hex'),
                        decode('000000000000000000000000000000f6', 'hex'),
                        decode('000000000000000000000000000000f8', 'hex'),
                        decode('000000000000000000000000000000f9', 'hex'),
                        decode('000000000000000000000000000000fa', 'hex'),
                        decode('000000000000000000000000000000fb', 'hex'),
                        decode('000000000000000000000000000000fc', 'hex'),
                        decode('000000000000000000000000000000fd', 'hex'),
                        decode('000000000000000000000000000000fe', 'hex'),
                        decode('000000000000000000000000000000ff', 'hex')
                    )
                    )
                )
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind = 'enum'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NOT NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind = 'record'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NOT NULL)
        ))
        OR (return_shape = 'stream' AND (
            (return_type_kind = 'scalar'
                AND return_scalar_type IS NOT NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind IN ('named', 'reference')
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NOT NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind = 'value'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NOT NULL
                AND (
                    (return_standard_library_revision_id IS NOT NULL
                     AND return_value_type_id NOT IN (
                        decode('000000000000000000000000000000f3', 'hex'),
                        decode('000000000000000000000000000000f4', 'hex'),
                        decode('000000000000000000000000000000f5', 'hex'),
                        decode('000000000000000000000000000000f6', 'hex'),
                        decode('000000000000000000000000000000f8', 'hex'),
                        decode('000000000000000000000000000000f9', 'hex'),
                        decode('000000000000000000000000000000fa', 'hex'),
                        decode('000000000000000000000000000000fb', 'hex'),
                        decode('000000000000000000000000000000fc', 'hex'),
                        decode('000000000000000000000000000000fd', 'hex'),
                        decode('000000000000000000000000000000fe', 'hex'),
                        decode('000000000000000000000000000000ff', 'hex')
                    )
                    )
                    OR (return_standard_library_revision_id IS NULL
                     AND return_value_type_id IN (
                        decode('000000000000000000000000000000f3', 'hex'),
                        decode('000000000000000000000000000000f4', 'hex'),
                        decode('000000000000000000000000000000f5', 'hex'),
                        decode('000000000000000000000000000000f6', 'hex'),
                        decode('000000000000000000000000000000f8', 'hex'),
                        decode('000000000000000000000000000000f9', 'hex'),
                        decode('000000000000000000000000000000fa', 'hex'),
                        decode('000000000000000000000000000000fb', 'hex'),
                        decode('000000000000000000000000000000fc', 'hex'),
                        decode('000000000000000000000000000000fd', 'hex'),
                        decode('000000000000000000000000000000fe', 'hex'),
                        decode('000000000000000000000000000000ff', 'hex')
                    )
                    )
                )
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind = 'enum'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NOT NULL
                AND return_record_type_id IS NULL)
            OR (return_type_kind = 'record'
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NULL
                AND return_value_type_id IS NULL
                AND return_standard_library_revision_id IS NULL
                AND return_enum_type_id IS NULL
                AND return_record_type_id IS NOT NULL)
        ))
    ),
    ADD CONSTRAINT catalogue_functions_return_value_pin_check CHECK (
        return_type_kind IS DISTINCT FROM 'value'
        OR (
            return_standard_library_revision_id IS NOT NULL
            AND return_value_type_id IS NOT NULL
            AND return_value_type_id NOT IN (
                decode('000000000000000000000000000000f3', 'hex'),
                decode('000000000000000000000000000000f4', 'hex'),
                decode('000000000000000000000000000000f5', 'hex'),
                decode('000000000000000000000000000000f6', 'hex'),
                decode('000000000000000000000000000000f8', 'hex'),
                decode('000000000000000000000000000000f9', 'hex'),
                decode('000000000000000000000000000000fa', 'hex'),
                decode('000000000000000000000000000000fb', 'hex'),
                decode('000000000000000000000000000000fc', 'hex'),
                decode('000000000000000000000000000000fd', 'hex'),
                decode('000000000000000000000000000000fe', 'hex'),
                decode('000000000000000000000000000000ff', 'hex')
            )
        )
        OR (return_standard_library_revision_id IS NULL
            AND return_value_type_id IS NOT NULL
            AND return_value_type_id IN (
                decode('000000000000000000000000000000f3', 'hex'),
                decode('000000000000000000000000000000f4', 'hex'),
                decode('000000000000000000000000000000f5', 'hex'),
                decode('000000000000000000000000000000f6', 'hex'),
                decode('000000000000000000000000000000f8', 'hex'),
                decode('000000000000000000000000000000f9', 'hex'),
                decode('000000000000000000000000000000fa', 'hex'),
                decode('000000000000000000000000000000fb', 'hex'),
                decode('000000000000000000000000000000fc', 'hex'),
                decode('000000000000000000000000000000fd', 'hex'),
                decode('000000000000000000000000000000fe', 'hex'),
                decode('000000000000000000000000000000ff', 'hex')
            )
        )
    );

ALTER TABLE _orna_kernel.definition_references
    DROP CONSTRAINT definition_references_target_std_lib_rev_shape_check,
    ADD CONSTRAINT definition_references_target_std_lib_rev_shape_check CHECK (
        (
            target_kind = 'value_type'
            AND (
                (target_standard_library_revision_id IS NOT NULL
                 AND target_definition_id NOT IN (
                    decode('000000000000000000000000000000f3', 'hex'),
                    decode('000000000000000000000000000000f4', 'hex'),
                    decode('000000000000000000000000000000f5', 'hex'),
                    decode('000000000000000000000000000000f6', 'hex'),
                    decode('000000000000000000000000000000f8', 'hex'),
                    decode('000000000000000000000000000000f9', 'hex'),
                    decode('000000000000000000000000000000fa', 'hex'),
                    decode('000000000000000000000000000000fb', 'hex'),
                    decode('000000000000000000000000000000fc', 'hex'),
                    decode('000000000000000000000000000000fd', 'hex'),
                    decode('000000000000000000000000000000fe', 'hex'),
                    decode('000000000000000000000000000000ff', 'hex')
                )
                )
                OR (target_standard_library_revision_id IS NULL
                 AND target_definition_id IN (
                    decode('000000000000000000000000000000f3', 'hex'),
                    decode('000000000000000000000000000000f4', 'hex'),
                    decode('000000000000000000000000000000f5', 'hex'),
                    decode('000000000000000000000000000000f6', 'hex'),
                    decode('000000000000000000000000000000f8', 'hex'),
                    decode('000000000000000000000000000000f9', 'hex'),
                    decode('000000000000000000000000000000fa', 'hex'),
                    decode('000000000000000000000000000000fb', 'hex'),
                    decode('000000000000000000000000000000fc', 'hex'),
                    decode('000000000000000000000000000000fd', 'hex'),
                    decode('000000000000000000000000000000fe', 'hex'),
                    decode('000000000000000000000000000000ff', 'hex')
                )
                )
            )
        )
        OR (
            target_kind <> 'value_type'
            AND target_standard_library_revision_id IS NULL
        )
    );
-- A sealed Inspector carrier in a REF signature has no application object row.
-- Project that one closed exception to a generated nullable FK key. Ordinary
-- named and REF targets keep the original deferred relational constraint.
ALTER TABLE _orna_kernel.catalogue_function_parameters
    DROP CONSTRAINT catalogue_function_parameters_catalogue_revision_id_target_fkey,
    ADD COLUMN target_type_id_fk bytea
        GENERATED ALWAYS AS (
            CASE
                WHEN type_kind = 'reference'
                 AND target_type_id = decode('000000000000000000000000000000f3', 'hex')
                THEN NULL
                ELSE target_type_id
            END
        ) STORED,
    ADD CONSTRAINT catalogue_function_parameters_catalogue_revision_id_target_fkey
        FOREIGN KEY (catalogue_revision_id, target_type_id_fk)
        REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE _orna_kernel.catalogue_functions
    DROP CONSTRAINT catalogue_functions_catalogue_revision_id_return_target_ty_fkey,
    ADD COLUMN return_target_type_id_fk bytea
        GENERATED ALWAYS AS (
            CASE
                WHEN return_type_kind = 'reference'
                 AND return_target_type_id = decode('000000000000000000000000000000f3', 'hex')
                THEN NULL
                ELSE return_target_type_id
            END
        ) STORED,
    ADD CONSTRAINT catalogue_functions_catalogue_revision_id_return_target_ty_fkey
        FOREIGN KEY (catalogue_revision_id, return_target_type_id_fk)
        REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED;
