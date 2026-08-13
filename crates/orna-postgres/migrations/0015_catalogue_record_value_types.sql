-- Store named immutable record definitions as protected catalogue data. The
-- fields remain Orna values; PostgreSQL does not create composite types or
-- assign semantic identities.

CREATE TABLE _orna_kernel.catalogue_record_value_types (
    catalogue_revision_id bytea NOT NULL,
    type_id bytea NOT NULL,
    schema_id bytea NOT NULL,
    name_parts text[] NOT NULL,
    value_kind text NOT NULL,
    mutability text NOT NULL,
    persistence text NOT NULL,
    source_unit_id bytea NOT NULL,
    source_start bigint NOT NULL,
    source_end bigint NOT NULL,
    CONSTRAINT catalogue_record_value_types_pkey
        PRIMARY KEY (catalogue_revision_id, type_id),
    CONSTRAINT cat_record_value_types_catalogue_revision_fk
        FOREIGN KEY (catalogue_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(id),
    CONSTRAINT cat_record_value_types_type_id_length
        CHECK (octet_length(type_id) = 16),
    CONSTRAINT cat_record_value_types_schema_id_length
        CHECK (octet_length(schema_id) = 16),
    CONSTRAINT cat_record_value_types_schema_fk
        FOREIGN KEY (catalogue_revision_id, schema_id)
        REFERENCES _orna_kernel.catalogue_schemas(catalogue_revision_id, schema_id),
    CONSTRAINT cat_record_value_types_name_parts_check CHECK (
        cardinality(name_parts) >= 2
        AND array_position(name_parts, NULL::text) IS NULL
        AND array_position(name_parts, '') IS NULL
    ),
    CONSTRAINT catalogue_record_value_types_name_key
        UNIQUE (catalogue_revision_id, name_parts),
    CONSTRAINT cat_record_value_types_value_kind_check
        CHECK (value_kind = 'record'),
    CONSTRAINT cat_record_value_types_mutability_check
        CHECK (mutability = 'immutable'),
    CONSTRAINT cat_record_value_types_persistence_check
        CHECK (persistence = 'persistable'),
    CONSTRAINT catalogue_record_value_types_source_origin_check CHECK (
        octet_length(source_unit_id) = 16
        AND source_start >= 0
        AND source_start <= 4294967295
        AND source_end >= source_start
        AND source_end <= 4294967295
    ),
    CONSTRAINT catalogue_record_value_types_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id)
);

CREATE TABLE _orna_kernel.catalogue_record_value_fields (
    catalogue_revision_id bytea NOT NULL,
    owner_type_id bytea NOT NULL,
    field_id bytea NOT NULL,
    name text NOT NULL,
    ordinal bigint NOT NULL,
    type_kind text NOT NULL,
    value_type_id bytea NULL,
    value_standard_library_revision_id bytea NULL,
    enum_type_id bytea NULL,
    source_unit_id bytea NOT NULL,
    source_start bigint NOT NULL,
    source_end bigint NOT NULL,
    CONSTRAINT catalogue_record_value_fields_pkey
        PRIMARY KEY (catalogue_revision_id, owner_type_id, field_id),
    CONSTRAINT catalogue_record_value_fields_name_key
        UNIQUE (catalogue_revision_id, owner_type_id, name),
    CONSTRAINT catalogue_record_value_fields_ordinal_key
        UNIQUE (catalogue_revision_id, owner_type_id, ordinal),
    CONSTRAINT cat_record_value_fields_owner_fk
        FOREIGN KEY (catalogue_revision_id, owner_type_id)
        REFERENCES _orna_kernel.catalogue_record_value_types(
            catalogue_revision_id,
            type_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT cat_record_value_fields_owner_id_length
        CHECK (octet_length(owner_type_id) = 16),
    CONSTRAINT cat_record_value_fields_field_id_length
        CHECK (octet_length(field_id) = 16),
    CONSTRAINT cat_record_value_fields_name_check
        CHECK (length(name) > 0),
    CONSTRAINT cat_record_value_fields_ordinal_check
        CHECK (ordinal >= 0 AND ordinal <= 4294967295),
    CONSTRAINT cat_record_value_fields_type_kind_check
        CHECK (type_kind IN ('value', 'enum')),
    CONSTRAINT cat_record_value_fields_type_check CHECK (
        (type_kind = 'value'
            AND value_type_id IS NOT NULL
            AND value_standard_library_revision_id IS NOT NULL
            AND enum_type_id IS NULL)
        OR (type_kind = 'enum'
            AND value_type_id IS NULL
            AND value_standard_library_revision_id IS NULL
            AND enum_type_id IS NOT NULL)
    ),
    CONSTRAINT cat_record_value_fields_value_type_id_length
        CHECK (value_type_id IS NULL OR octet_length(value_type_id) = 16),
    CONSTRAINT cat_record_value_fields_value_revision_length CHECK (
        value_standard_library_revision_id IS NULL
        OR octet_length(value_standard_library_revision_id) = 16
    ),
    CONSTRAINT cat_record_value_fields_enum_type_id_length
        CHECK (enum_type_id IS NULL OR octet_length(enum_type_id) = 16),
    CONSTRAINT cat_record_value_fields_value_pin_fk
        FOREIGN KEY (catalogue_revision_id, value_standard_library_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(
            id,
            standard_library_revision_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT cat_record_value_fields_value_type_fk
        FOREIGN KEY (value_standard_library_revision_id, value_type_id)
        REFERENCES _orna_kernel.standard_catalogue_value_types(
            standard_library_revision_id,
            type_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT cat_record_value_fields_enum_type_fk
        FOREIGN KEY (catalogue_revision_id, enum_type_id)
        REFERENCES _orna_kernel.catalogue_enum_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT catalogue_record_value_fields_source_origin_check CHECK (
        octet_length(source_unit_id) = 16
        AND source_start >= 0
        AND source_start <= 4294967295
        AND source_end >= source_start
        AND source_end <= 4294967295
    ),
    CONSTRAINT catalogue_record_value_fields_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id)
);

REVOKE ALL ON TABLE _orna_kernel.catalogue_record_value_types FROM PUBLIC;
REVOKE ALL ON TABLE _orna_kernel.catalogue_record_value_fields FROM PUBLIC;
