-- Store pinned standard enum definitions and retain enum provenance in record
-- fields. Existing application-enum and standard-primitive rows keep their
-- exact shape and meaning.

CREATE TABLE _orna_kernel.standard_catalogue_enum_types (
    standard_library_revision_id bytea NOT NULL,
    type_id bytea NOT NULL,
    schema_id bytea NOT NULL,
    name_parts text[] NOT NULL,
    labels text[] NOT NULL,
    source_unit_id bytea NOT NULL,
    source_start bigint NOT NULL,
    source_end bigint NOT NULL,
    CONSTRAINT std_cat_enum_types_pkey
        PRIMARY KEY (standard_library_revision_id, type_id),
    CONSTRAINT std_cat_enum_types_std_lib_rev_id_length
        CHECK (octet_length(standard_library_revision_id) = 16),
    CONSTRAINT std_cat_enum_types_std_lib_rev_fk
        FOREIGN KEY (standard_library_revision_id)
        REFERENCES _orna_kernel.standard_library_revisions(id),
    CONSTRAINT std_cat_enum_types_type_id_length
        CHECK (octet_length(type_id) = 16),
    CONSTRAINT std_cat_enum_types_schema_id_length
        CHECK (octet_length(schema_id) = 16),
    CONSTRAINT std_cat_enum_types_schema_fk
        FOREIGN KEY (standard_library_revision_id, schema_id)
        REFERENCES _orna_kernel.standard_catalogue_schemas(
            standard_library_revision_id,
            schema_id
        ),
    CONSTRAINT std_cat_enum_types_name_parts_check CHECK (
        cardinality(name_parts) >= 2
        AND array_position(name_parts, NULL::text) IS NULL
        AND array_position(name_parts, '') IS NULL
    ),
    CONSTRAINT std_cat_enum_types_name_key
        UNIQUE (standard_library_revision_id, name_parts),
    CONSTRAINT std_cat_enum_types_labels_check CHECK (
        cardinality(labels) > 0
        AND array_position(labels, NULL::text) IS NULL
    ),
    CONSTRAINT standard_catalogue_enum_types_source_origin_check CHECK (
        octet_length(source_unit_id) = 16
        AND source_start >= 0
        AND source_start <= 4294967295
        AND source_end >= source_start
        AND source_end <= 4294967295
    ),
    CONSTRAINT standard_catalogue_enum_types_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id)
);

ALTER TABLE _orna_kernel.standard_catalogue_type_bindings
    ADD COLUMN target_type_kind text NOT NULL DEFAULT 'value',
    ADD COLUMN target_enum_type_id bytea NULL,
    ALTER COLUMN target_type_id DROP NOT NULL,
    DROP CONSTRAINT std_cat_type_bindings_target_type_fk,
    ADD CONSTRAINT std_cat_type_bindings_target_type_kind_check
        CHECK (target_type_kind IN ('value', 'enum')),
    ADD CONSTRAINT std_cat_type_bindings_target_shape_check CHECK (
        (target_type_kind = 'value'
            AND target_type_id IS NOT NULL
            AND target_enum_type_id IS NULL)
        OR (target_type_kind = 'enum'
            AND target_type_id IS NULL
            AND target_enum_type_id IS NOT NULL)
    ),
    ADD CONSTRAINT std_cat_type_bindings_target_enum_id_length
        CHECK (target_enum_type_id IS NULL OR octet_length(target_enum_type_id) = 16),
    ADD CONSTRAINT std_cat_type_bindings_target_type_fk
        FOREIGN KEY (standard_library_revision_id, target_type_id)
        REFERENCES _orna_kernel.standard_catalogue_value_types(
            standard_library_revision_id,
            type_id
        ),
    ADD CONSTRAINT std_cat_type_bindings_target_enum_fk
        FOREIGN KEY (standard_library_revision_id, target_enum_type_id)
        REFERENCES _orna_kernel.standard_catalogue_enum_types(
            standard_library_revision_id,
            type_id
        );

ALTER TABLE _orna_kernel.catalogue_record_value_fields
    ADD COLUMN enum_standard_library_revision_id bytea NULL,
    ADD COLUMN standard_enum_type_id bytea NULL,
    DROP CONSTRAINT cat_record_value_fields_type_check,
    ADD CONSTRAINT cat_record_value_fields_type_check CHECK (
        (type_kind = 'value'
            AND value_type_id IS NOT NULL
            AND value_standard_library_revision_id IS NOT NULL
            AND enum_type_id IS NULL
            AND enum_standard_library_revision_id IS NULL
            AND standard_enum_type_id IS NULL)
        OR (type_kind = 'enum'
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NOT NULL
            AND enum_standard_library_revision_id IS NULL
            AND standard_enum_type_id IS NULL)
        OR (type_kind = 'enum'
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NULL
            AND enum_standard_library_revision_id IS NOT NULL
            AND standard_enum_type_id IS NOT NULL)
    ),
    ADD CONSTRAINT cat_record_value_fields_enum_std_rev_length CHECK (
        enum_standard_library_revision_id IS NULL
        OR octet_length(enum_standard_library_revision_id) = 16
    ),
    ADD CONSTRAINT cat_record_value_fields_std_enum_id_length CHECK (
        standard_enum_type_id IS NULL
        OR octet_length(standard_enum_type_id) = 16
    ),
    ADD CONSTRAINT cat_record_value_fields_enum_pin_fk
        FOREIGN KEY (catalogue_revision_id, enum_standard_library_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(
            id,
            standard_library_revision_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT cat_record_value_fields_std_enum_fk
        FOREIGN KEY (enum_standard_library_revision_id, standard_enum_type_id)
        REFERENCES _orna_kernel.standard_catalogue_enum_types(
            standard_library_revision_id,
            type_id
        )
        DEFERRABLE INITIALLY DEFERRED;

REVOKE ALL ON TABLE _orna_kernel.standard_catalogue_enum_types FROM PUBLIC;
