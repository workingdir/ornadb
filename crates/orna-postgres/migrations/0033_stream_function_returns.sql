-- Allow application server functions to return typed streams without
-- changing the closed return-shape contract of standard snapshots.
ALTER TABLE _orna_kernel.catalogue_functions
    DROP CONSTRAINT catalogue_functions_return_shape_check,
    DROP CONSTRAINT catalogue_functions_check1,
    ADD CONSTRAINT catalogue_functions_return_shape_check
        CHECK (return_shape IN ('single', 'rows', 'stream')),
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
        OR (return_shape = 'stream' AND (
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
    ADD CONSTRAINT catalogue_functions_return_kind_presence_check CHECK (
        (return_shape = 'rows' AND return_type_kind IS NULL)
        OR (return_shape IN ('single', 'stream') AND return_type_kind IS NOT NULL)
    ),
    ADD CONSTRAINT catalogue_functions_stream_void_check CHECK (
        return_shape <> 'stream'
        OR return_type_kind <> 'scalar'
        OR return_scalar_type IS DISTINCT FROM 'void'
    );
