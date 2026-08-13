-- Store an application record target for one nested immutable record field.
-- Existing value and enum tuples remain exact and cannot carry this target.

ALTER TABLE _orna_kernel.catalogue_record_value_fields
    ADD COLUMN record_type_id bytea NULL,
    DROP CONSTRAINT cat_record_value_fields_type_kind_check,
    ADD CONSTRAINT cat_record_value_fields_type_kind_check
        CHECK (type_kind IN ('value', 'enum', 'record')),
    DROP CONSTRAINT cat_record_value_fields_type_check,
    ADD CONSTRAINT cat_record_value_fields_type_check CHECK (
        (type_kind = 'value'
            AND value_type_id IS NOT NULL
            AND value_standard_library_revision_id IS NOT NULL
            AND enum_type_id IS NULL
            AND enum_standard_library_revision_id IS NULL
            AND standard_enum_type_id IS NULL
            AND record_type_id IS NULL)
        OR (type_kind = 'enum'
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NOT NULL
            AND enum_standard_library_revision_id IS NULL
            AND standard_enum_type_id IS NULL
            AND record_type_id IS NULL)
        OR (type_kind = 'enum'
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL
            AND enum_standard_library_revision_id IS NOT NULL
            AND standard_enum_type_id IS NOT NULL
            AND record_type_id IS NULL)
        OR (type_kind = 'record'
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL
            AND enum_standard_library_revision_id IS NULL
            AND standard_enum_type_id IS NULL
            AND record_type_id IS NOT NULL)
    ),
    ADD CONSTRAINT cat_record_value_fields_record_type_id_length
        CHECK (record_type_id IS NULL OR octet_length(record_type_id) = 16),
    ADD CONSTRAINT cat_record_value_fields_record_type_fk
        FOREIGN KEY (catalogue_revision_id, record_type_id)
        REFERENCES _orna_kernel.catalogue_record_value_types(
            catalogue_revision_id,
            type_id
        )
        DEFERRABLE INITIALLY DEFERRED;

REVOKE ALL ON TABLE _orna_kernel.catalogue_record_value_fields FROM PUBLIC;
