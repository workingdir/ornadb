-- Store only the durable schema needed for standard catalogue types. This
-- migration creates no standard data and has no migration data step.

CREATE TABLE _orna_kernel.standard_library_revisions (
    id bytea NOT NULL,
    source_revision_id bytea NOT NULL,
    catalogue_revision_id bytea NOT NULL,
    digest_version smallint NOT NULL DEFAULT 1,
    language_version text NOT NULL,
    content_hash bytea NOT NULL,
    hash_algorithm text NOT NULL DEFAULT 'sha256',
    created_at timestamp with time zone NOT NULL DEFAULT transaction_timestamp(),
    CONSTRAINT std_lib_rev_pkey PRIMARY KEY (id),
    CONSTRAINT std_lib_rev_id_length CHECK (octet_length(id) = 16),
    CONSTRAINT std_lib_rev_source_revision_id_length
        CHECK (octet_length(source_revision_id) = 16),
    CONSTRAINT std_lib_rev_source_revision_key UNIQUE (source_revision_id),
    CONSTRAINT std_lib_rev_source_revision_fk
        FOREIGN KEY (source_revision_id)
        REFERENCES _orna_kernel.source_revisions(id),
    CONSTRAINT std_lib_rev_catalogue_revision_id_length
        CHECK (octet_length(catalogue_revision_id) = 16),
    CONSTRAINT std_lib_rev_catalogue_revision_key UNIQUE (catalogue_revision_id),
    CONSTRAINT std_lib_rev_digest_version_check CHECK (digest_version = 1),
    CONSTRAINT std_lib_rev_language_version_check CHECK (length(language_version) > 0),
    CONSTRAINT std_lib_rev_content_hash_length CHECK (octet_length(content_hash) = 32),
    CONSTRAINT std_lib_rev_hash_algorithm_check CHECK (hash_algorithm = 'sha256')
);

CREATE TABLE _orna_kernel.standard_catalogue_schemas (
    standard_library_revision_id bytea NOT NULL,
    schema_id bytea NOT NULL,
    name_parts text[] NOT NULL,
    source_unit_id bytea NOT NULL,
    source_start bigint NOT NULL,
    source_end bigint NOT NULL,
    CONSTRAINT std_cat_schemas_pkey
        PRIMARY KEY (standard_library_revision_id, schema_id),
    CONSTRAINT std_cat_schemas_std_lib_rev_id_length
        CHECK (octet_length(standard_library_revision_id) = 16),
    CONSTRAINT std_cat_schemas_std_lib_rev_fk
        FOREIGN KEY (standard_library_revision_id)
        REFERENCES _orna_kernel.standard_library_revisions(id),
    CONSTRAINT std_cat_schemas_schema_id_length CHECK (octet_length(schema_id) = 16),
    CONSTRAINT std_cat_schemas_name_parts_check CHECK (
        cardinality(name_parts) > 0
        AND array_position(name_parts, NULL::text) IS NULL
        AND array_position(name_parts, '') IS NULL
    ),
    CONSTRAINT std_cat_schemas_name_key UNIQUE (standard_library_revision_id, name_parts),
    CONSTRAINT std_cat_schemas_source_origin_check CHECK (
        octet_length(source_unit_id) = 16
        AND source_start >= 0
        AND source_start <= 4294967295
        AND source_end >= source_start
        AND source_end <= 4294967295
    ),
    CONSTRAINT std_cat_schemas_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id)
);

CREATE TABLE _orna_kernel.standard_catalogue_value_types (
    standard_library_revision_id bytea NOT NULL,
    type_id bytea NOT NULL,
    schema_id bytea NOT NULL,
    name_parts text[] NOT NULL,
    value_kind text NOT NULL,
    mutability text NOT NULL,
    persistence text NOT NULL,
    representation_contract text NOT NULL,
    source_unit_id bytea NOT NULL,
    source_start bigint NOT NULL,
    source_end bigint NOT NULL,
    CONSTRAINT std_cat_value_types_pkey
        PRIMARY KEY (standard_library_revision_id, type_id),
    CONSTRAINT std_cat_value_types_std_lib_rev_id_length
        CHECK (octet_length(standard_library_revision_id) = 16),
    CONSTRAINT std_cat_value_types_std_lib_rev_fk
        FOREIGN KEY (standard_library_revision_id)
        REFERENCES _orna_kernel.standard_library_revisions(id),
    CONSTRAINT std_cat_value_types_type_id_length CHECK (octet_length(type_id) = 16),
    CONSTRAINT std_cat_value_types_schema_id_length CHECK (octet_length(schema_id) = 16),
    CONSTRAINT std_cat_value_types_schema_fk
        FOREIGN KEY (standard_library_revision_id, schema_id)
        REFERENCES _orna_kernel.standard_catalogue_schemas(
            standard_library_revision_id,
            schema_id
        ),
    CONSTRAINT std_cat_value_types_name_parts_check CHECK (
        cardinality(name_parts) >= 2
        AND array_position(name_parts, NULL::text) IS NULL
        AND array_position(name_parts, '') IS NULL
    ),
    CONSTRAINT std_cat_value_types_name_key
        UNIQUE (standard_library_revision_id, name_parts),
    CONSTRAINT std_cat_value_types_value_kind_check CHECK (value_kind = 'primitive'),
    CONSTRAINT std_cat_value_types_mutability_check CHECK (mutability = 'immutable'),
    CONSTRAINT std_cat_value_types_persistence_check
        CHECK (persistence IN ('persistable', 'transient')),
    CONSTRAINT std_cat_value_types_representation_contract_check
        CHECK (length(representation_contract) > 0),
    CONSTRAINT std_cat_value_types_source_origin_check CHECK (
        octet_length(source_unit_id) = 16
        AND source_start >= 0
        AND source_start <= 4294967295
        AND source_end >= source_start
        AND source_end <= 4294967295
    ),
    CONSTRAINT std_cat_value_types_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id)
);

CREATE TABLE _orna_kernel.standard_catalogue_type_bindings (
    standard_library_revision_id bytea NOT NULL,
    type_binding_id bytea NOT NULL,
    kind text NOT NULL,
    name_parts text[] NOT NULL,
    target_type_id bytea NOT NULL,
    source_unit_id bytea NOT NULL,
    source_start bigint NOT NULL,
    source_end bigint NOT NULL,
    CONSTRAINT std_cat_type_bindings_pkey
        PRIMARY KEY (standard_library_revision_id, type_binding_id),
    CONSTRAINT std_cat_type_bindings_std_lib_rev_id_length
        CHECK (octet_length(standard_library_revision_id) = 16),
    CONSTRAINT std_cat_type_bindings_std_lib_rev_fk
        FOREIGN KEY (standard_library_revision_id)
        REFERENCES _orna_kernel.standard_library_revisions(id),
    CONSTRAINT std_cat_type_bindings_type_binding_id_length
        CHECK (octet_length(type_binding_id) = 16),
    CONSTRAINT std_cat_type_bindings_kind_check
        CHECK (kind IN ('qualified', 'prelude')),
    CONSTRAINT std_cat_type_bindings_name_parts_check CHECK (
        (
            kind = 'qualified'
            AND cardinality(name_parts) >= 2
            AND array_position(name_parts, NULL::text) IS NULL
            AND array_position(name_parts, '') IS NULL
        )
        OR (
            kind = 'prelude'
            AND cardinality(name_parts) >= 1
            AND array_position(name_parts, NULL::text) IS NULL
            AND array_position(name_parts, '') IS NULL
        )
    ),
    CONSTRAINT std_cat_type_bindings_name_key
        UNIQUE (standard_library_revision_id, kind, name_parts),
    CONSTRAINT std_cat_type_bindings_target_type_id_length
        CHECK (octet_length(target_type_id) = 16),
    CONSTRAINT std_cat_type_bindings_target_type_fk
        FOREIGN KEY (standard_library_revision_id, target_type_id)
        REFERENCES _orna_kernel.standard_catalogue_value_types(
            standard_library_revision_id,
            type_id
        ),
    CONSTRAINT std_cat_type_bindings_source_origin_check CHECK (
        octet_length(source_unit_id) = 16
        AND source_start >= 0
        AND source_start <= 4294967295
        AND source_end >= source_start
        AND source_end <= 4294967295
    ),
    CONSTRAINT std_cat_type_bindings_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id)
);

ALTER TABLE _orna_kernel.catalogue_revisions
    ADD COLUMN canonical_hash_version smallint NOT NULL DEFAULT 1,
    ADD COLUMN standard_library_revision_id bytea NULL,
    ADD CONSTRAINT catalogue_revisions_canonical_hash_version_check
        CHECK (canonical_hash_version IN (1, 2)),
    ADD CONSTRAINT catalogue_revisions_std_lib_rev_id_length
        CHECK (
            standard_library_revision_id IS NULL
            OR octet_length(standard_library_revision_id) = 16
        ),
    ADD CONSTRAINT catalogue_revisions_std_lib_rev_fk
        FOREIGN KEY (standard_library_revision_id)
        REFERENCES _orna_kernel.standard_library_revisions(id),
    ADD CONSTRAINT catalogue_revisions_standard_context_check CHECK (
        (canonical_hash_version = 1 AND standard_library_revision_id IS NULL)
        OR (canonical_hash_version = 2 AND standard_library_revision_id IS NOT NULL)
    ),
    ADD CONSTRAINT catalogue_revisions_id_std_lib_rev_key
        UNIQUE (id, standard_library_revision_id);

ALTER TABLE _orna_kernel.function_revisions
    ADD COLUMN semantic_hash_version smallint NOT NULL DEFAULT 1,
    ADD CONSTRAINT function_revisions_semantic_hash_version_check
        CHECK (semantic_hash_version IN (1, 2));

ALTER TABLE _orna_kernel.definition_references
    ADD COLUMN target_standard_library_revision_id bytea NULL,
    ADD CONSTRAINT definition_references_target_std_lib_rev_id_length
        CHECK (
            target_standard_library_revision_id IS NULL
            OR octet_length(target_standard_library_revision_id) = 16
        ),
    DROP CONSTRAINT definition_references_target_kind_check,
    DROP CONSTRAINT definition_references_target_owner_shape_check,
    DROP CONSTRAINT definition_references_reference_target_compatibility_check,
    ADD CONSTRAINT definition_references_target_kind_check CHECK (target_kind IN (
        'object_type',
        'field',
        'function',
        'parameter',
        'expression',
        'value_type'
    )),
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
            target_kind = 'value_type'
            AND target_owner_type_id IS NULL
            AND target_owner_function_id IS NULL
        )
        OR (
            target_kind NOT IN ('field', 'parameter', 'value_type')
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
        OR (reference_kind = 'write_object' AND target_kind = 'object_type')
        OR (reference_kind = 'write_field' AND target_kind = 'field')
        OR (reference_kind = 'named_type' AND target_kind = 'value_type')
    ),
    ADD CONSTRAINT definition_references_target_std_lib_rev_shape_check CHECK (
        (
            target_kind = 'value_type'
            AND target_standard_library_revision_id IS NOT NULL
        )
        OR (
            target_kind <> 'value_type'
            AND target_standard_library_revision_id IS NULL
        )
    ),
    ADD CONSTRAINT definition_references_catalogue_std_lib_rev_fk
        FOREIGN KEY (
            catalogue_revision_id,
            target_standard_library_revision_id
        )
        REFERENCES _orna_kernel.catalogue_revisions(
            id,
            standard_library_revision_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT definition_references_std_value_type_target_fk
        FOREIGN KEY (
            target_standard_library_revision_id,
            target_definition_id
        )
        REFERENCES _orna_kernel.standard_catalogue_value_types(
            standard_library_revision_id,
            type_id
        )
        DEFERRABLE INITIALLY DEFERRED;

CREATE INDEX catalogue_schemas_identity_index
    ON _orna_kernel.catalogue_schemas (schema_id, catalogue_revision_id);

CREATE INDEX catalogue_object_types_identity_index
    ON _orna_kernel.catalogue_object_types (type_id, catalogue_revision_id);

CREATE INDEX standard_catalogue_schemas_identity_index
    ON _orna_kernel.standard_catalogue_schemas (schema_id, standard_library_revision_id);

CREATE INDEX standard_catalogue_value_types_identity_index
    ON _orna_kernel.standard_catalogue_value_types (type_id, standard_library_revision_id);

CREATE INDEX standard_catalogue_type_bindings_identity_index
    ON _orna_kernel.standard_catalogue_type_bindings (
        type_binding_id,
        standard_library_revision_id
    );

CREATE INDEX definition_references_value_type_target_index
    ON _orna_kernel.definition_references (
        target_standard_library_revision_id,
        target_definition_id,
        catalogue_revision_id
    )
    WHERE target_kind = 'value_type';

REVOKE ALL ON TABLE _orna_kernel.standard_library_revisions FROM PUBLIC;
REVOKE ALL ON TABLE _orna_kernel.standard_catalogue_schemas FROM PUBLIC;
REVOKE ALL ON TABLE _orna_kernel.standard_catalogue_value_types FROM PUBLIC;
REVOKE ALL ON TABLE _orna_kernel.standard_catalogue_type_bindings FROM PUBLIC;
