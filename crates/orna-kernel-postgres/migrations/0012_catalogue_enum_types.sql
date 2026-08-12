-- Store application enum definitions as protected catalogue data. PostgreSQL
-- text arrays preserve the semantic label order without creating PostgreSQL
-- enum types or allocating separate label identities.

CREATE TABLE _orna_kernel.catalogue_enum_types (
    catalogue_revision_id bytea NOT NULL,
    type_id bytea NOT NULL,
    schema_id bytea NOT NULL,
    name_parts text[] NOT NULL,
    labels text[] NOT NULL,
    source_unit_id bytea NOT NULL,
    source_start bigint NOT NULL,
    source_end bigint NOT NULL,
    CONSTRAINT catalogue_enum_types_pkey
        PRIMARY KEY (catalogue_revision_id, type_id),
    CONSTRAINT catalogue_enum_types_catalogue_revision_fk
        FOREIGN KEY (catalogue_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(id),
    CONSTRAINT catalogue_enum_types_type_id_length
        CHECK (octet_length(type_id) = 16),
    CONSTRAINT catalogue_enum_types_schema_id_length
        CHECK (octet_length(schema_id) = 16),
    CONSTRAINT catalogue_enum_types_schema_fk
        FOREIGN KEY (catalogue_revision_id, schema_id)
        REFERENCES _orna_kernel.catalogue_schemas(catalogue_revision_id, schema_id),
    CONSTRAINT catalogue_enum_types_name_parts_check CHECK (
        cardinality(name_parts) >= 2
        AND array_position(name_parts, NULL::text) IS NULL
        AND array_position(name_parts, '') IS NULL
    ),
    CONSTRAINT catalogue_enum_types_name_key
        UNIQUE (catalogue_revision_id, name_parts),
    CONSTRAINT catalogue_enum_types_labels_check CHECK (
        cardinality(labels) > 0
        AND array_position(labels, NULL::text) IS NULL
    ),
    CONSTRAINT catalogue_enum_types_source_origin_check CHECK (
        octet_length(source_unit_id) = 16
        AND source_start >= 0
        AND source_start <= 4294967295
        AND source_end >= source_start
        AND source_end <= 4294967295
    ),
    CONSTRAINT catalogue_enum_types_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id)
);

REVOKE ALL ON TABLE _orna_kernel.catalogue_enum_types FROM PUBLIC;
