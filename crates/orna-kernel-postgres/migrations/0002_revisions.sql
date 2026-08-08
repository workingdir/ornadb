-- Preserve revision-wide integrity data that was absent from the first
-- bootstrap schema. Migration 0001 can only have created an empty seed.

ALTER TABLE _orna_kernel.source_bundles
    ADD COLUMN content_hash bytea,
    ADD COLUMN hash_algorithm text NOT NULL DEFAULT 'sha256'
        CHECK (hash_algorithm = 'sha256');

ALTER TABLE _orna_kernel.source_revisions
    ADD COLUMN content_hash bytea,
    ADD COLUMN hash_algorithm text NOT NULL DEFAULT 'sha256'
        CHECK (hash_algorithm = 'sha256');

ALTER TABLE _orna_kernel.catalogue_revisions
    ADD COLUMN content_hash bytea,
    ADD COLUMN hash_algorithm text NOT NULL DEFAULT 'sha256'
        CHECK (hash_algorithm = 'sha256');

DO $$
DECLARE
    bundle_count bigint;
    source_revision_count bigint;
    catalogue_revision_count bigint;
    source_unit_count bigint;
    active_revision_count bigint;
    schema_count bigint;
    object_type_count bigint;
    field_count bigint;
    expression_count bigint;
    function_count bigint;
    parameter_count bigint;
    return_column_count bigint;
    function_revision_count bigint;
    function_artifact_count bigint;
    data_relation_count bigint;
BEGIN
    SELECT count(*) INTO bundle_count FROM _orna_kernel.source_bundles;
    SELECT count(*) INTO source_revision_count FROM _orna_kernel.source_revisions;
    SELECT count(*) INTO catalogue_revision_count FROM _orna_kernel.catalogue_revisions;
    SELECT count(*) INTO source_unit_count FROM _orna_kernel.source_units;
    SELECT count(*) INTO active_revision_count FROM _orna_kernel.active_revision;
    SELECT count(*) INTO schema_count FROM _orna_kernel.catalogue_schemas;
    SELECT count(*) INTO object_type_count FROM _orna_kernel.catalogue_object_types;
    SELECT count(*) INTO field_count FROM _orna_kernel.catalogue_fields;
    SELECT count(*) INTO expression_count FROM _orna_kernel.catalogue_expressions;
    SELECT count(*) INTO function_count FROM _orna_kernel.catalogue_functions;
    SELECT count(*) INTO parameter_count FROM _orna_kernel.catalogue_function_parameters;
    SELECT count(*) INTO return_column_count
    FROM _orna_kernel.catalogue_function_return_columns;
    SELECT count(*) INTO function_revision_count FROM _orna_kernel.function_revisions;
    SELECT count(*) INTO function_artifact_count FROM _orna_kernel.function_artifacts;
    SELECT count(*) INTO data_relation_count
    FROM pg_class AS relation
    JOIN pg_namespace AS namespace ON namespace.oid = relation.relnamespace
    WHERE namespace.nspname = '_orna_data'
      AND relation.relkind IN ('r', 'p');

    IF bundle_count = 0
        AND source_revision_count = 0
        AND catalogue_revision_count = 0
        AND source_unit_count = 0
        AND active_revision_count = 0
        AND schema_count = 0
        AND object_type_count = 0
        AND field_count = 0
        AND expression_count = 0
        AND function_count = 0
        AND parameter_count = 0
        AND return_column_count = 0
        AND function_revision_count = 0
        AND function_artifact_count = 0
        AND data_relation_count = 0 THEN
        NULL;
    ELSIF bundle_count = 1
        AND source_revision_count = 1
        AND catalogue_revision_count = 1
        AND source_unit_count = 0
        AND active_revision_count = 1
        AND schema_count = 0
        AND object_type_count = 0
        AND field_count = 0
        AND expression_count = 0
        AND function_count = 0
        AND parameter_count = 0
        AND return_column_count = 0
        AND function_revision_count = 0
        AND function_artifact_count = 0
        AND data_relation_count = 0 THEN
        UPDATE _orna_kernel.source_bundles
        SET content_hash = decode(
            'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
            'hex'
        );
        UPDATE _orna_kernel.source_revisions
        SET content_hash = decode(
            'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
            'hex'
        );
        UPDATE _orna_kernel.catalogue_revisions
        SET content_hash = decode(
            'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855',
            'hex'
        );
    ELSE
        RAISE EXCEPTION
            'cannot derive aggregate hashes for a non-empty migration 0001 catalogue';
    END IF;
END;
$$;

ALTER TABLE _orna_kernel.source_bundles
    ADD CONSTRAINT source_bundles_content_hash_length
        CHECK (octet_length(content_hash) = 32),
    ALTER COLUMN content_hash SET NOT NULL;

ALTER TABLE _orna_kernel.source_revisions
    ADD CONSTRAINT source_revisions_content_hash_length
        CHECK (octet_length(content_hash) = 32),
    ALTER COLUMN content_hash SET NOT NULL;

ALTER TABLE _orna_kernel.catalogue_revisions
    ADD CONSTRAINT catalogue_revisions_content_hash_length
        CHECK (octet_length(content_hash) = 32),
    ALTER COLUMN content_hash SET NOT NULL;

-- Source units have globally stable identities. These foreign keys prove that
-- an origin exists. Recovery additionally checks that the origin belongs to
-- the source bundle selected by the catalogue revision.
ALTER TABLE _orna_kernel.catalogue_schemas
    ADD COLUMN source_unit_id bytea,
    ADD COLUMN source_start bigint,
    ADD COLUMN source_end bigint,
    ADD CONSTRAINT catalogue_schemas_source_origin_check CHECK (
        (source_unit_id IS NULL AND source_start IS NULL AND source_end IS NULL)
        OR (
            source_unit_id IS NOT NULL
            AND octet_length(source_unit_id) = 16
            AND source_start IS NOT NULL
            AND source_end IS NOT NULL
            AND source_start >= 0
            AND source_end >= source_start
        )
    ),
    ADD CONSTRAINT catalogue_schemas_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id);

ALTER TABLE _orna_kernel.catalogue_object_types
    ADD COLUMN source_unit_id bytea,
    ADD COLUMN source_start bigint,
    ADD COLUMN source_end bigint,
    ADD CONSTRAINT catalogue_object_types_source_origin_check CHECK (
        (source_unit_id IS NULL AND source_start IS NULL AND source_end IS NULL)
        OR (
            source_unit_id IS NOT NULL
            AND octet_length(source_unit_id) = 16
            AND source_start IS NOT NULL
            AND source_end IS NOT NULL
            AND source_start >= 0
            AND source_end >= source_start
        )
    ),
    ADD CONSTRAINT catalogue_object_types_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id);

ALTER TABLE _orna_kernel.catalogue_fields
    ADD COLUMN source_unit_id bytea,
    ADD COLUMN source_start bigint,
    ADD COLUMN source_end bigint,
    ADD CONSTRAINT catalogue_fields_source_origin_check CHECK (
        (source_unit_id IS NULL AND source_start IS NULL AND source_end IS NULL)
        OR (
            source_unit_id IS NOT NULL
            AND octet_length(source_unit_id) = 16
            AND source_start IS NOT NULL
            AND source_end IS NOT NULL
            AND source_start >= 0
            AND source_end >= source_start
        )
    ),
    ADD CONSTRAINT catalogue_fields_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id);

ALTER TABLE _orna_kernel.catalogue_expressions
    ADD COLUMN source_unit_id bytea,
    ADD COLUMN source_start bigint,
    ADD COLUMN source_end bigint,
    ADD CONSTRAINT catalogue_expressions_source_origin_check CHECK (
        (source_unit_id IS NULL AND source_start IS NULL AND source_end IS NULL)
        OR (
            source_unit_id IS NOT NULL
            AND octet_length(source_unit_id) = 16
            AND source_start IS NOT NULL
            AND source_end IS NOT NULL
            AND source_start >= 0
            AND source_end >= source_start
        )
    ),
    ADD CONSTRAINT catalogue_expressions_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id);

ALTER TABLE _orna_kernel.catalogue_functions
    ADD COLUMN source_unit_id bytea,
    ADD COLUMN source_start bigint,
    ADD COLUMN source_end bigint,
    ADD CONSTRAINT catalogue_functions_source_origin_check CHECK (
        (source_unit_id IS NULL AND source_start IS NULL AND source_end IS NULL)
        OR (
            source_unit_id IS NOT NULL
            AND octet_length(source_unit_id) = 16
            AND source_start IS NOT NULL
            AND source_end IS NOT NULL
            AND source_start >= 0
            AND source_end >= source_start
        )
    ),
    ADD CONSTRAINT catalogue_functions_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id);

ALTER TABLE _orna_kernel.catalogue_function_parameters
    ADD COLUMN source_unit_id bytea,
    ADD COLUMN source_start bigint,
    ADD COLUMN source_end bigint,
    ADD CONSTRAINT catalogue_function_parameters_source_origin_check CHECK (
        (source_unit_id IS NULL AND source_start IS NULL AND source_end IS NULL)
        OR (
            source_unit_id IS NOT NULL
            AND octet_length(source_unit_id) = 16
            AND source_start IS NOT NULL
            AND source_end IS NOT NULL
            AND source_start >= 0
            AND source_end >= source_start
        )
    ),
    ADD CONSTRAINT catalogue_function_parameters_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id);

ALTER TABLE _orna_kernel.catalogue_function_return_columns
    ADD COLUMN source_unit_id bytea,
    ADD COLUMN source_start bigint,
    ADD COLUMN source_end bigint,
    ADD CONSTRAINT catalogue_function_return_columns_source_origin_check CHECK (
        (source_unit_id IS NULL AND source_start IS NULL AND source_end IS NULL)
        OR (
            source_unit_id IS NOT NULL
            AND octet_length(source_unit_id) = 16
            AND source_start IS NOT NULL
            AND source_end IS NOT NULL
            AND source_start >= 0
            AND source_end >= source_start
        )
    ),
    ADD CONSTRAINT catalogue_function_return_columns_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id);

ALTER TABLE _orna_kernel.catalogue_functions
    DROP CONSTRAINT catalogue_functions_current_revision_fk;

ALTER TABLE _orna_kernel.function_revisions
    RENAME COLUMN catalogue_revision_id TO introduced_catalogue_revision_id;

ALTER TABLE _orna_kernel.function_revisions
    DROP CONSTRAINT function_revisions_catalogue_revision_id_function_id_fkey,
    DROP CONSTRAINT function_revisions_catalogue_revision_id_function_id_id_key,
    ADD CONSTRAINT function_revisions_introduced_catalogue_revision_fk
        FOREIGN KEY (introduced_catalogue_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(id),
    ADD CONSTRAINT function_revisions_introduced_function_fk
        FOREIGN KEY (introduced_catalogue_revision_id, function_id)
        REFERENCES _orna_kernel.catalogue_functions(catalogue_revision_id, function_id)
        DEFERRABLE INITIALLY DEFERRED,
    ADD CONSTRAINT function_revisions_function_id_id_key
        UNIQUE (function_id, id);

ALTER TABLE _orna_kernel.catalogue_functions
    ADD CONSTRAINT catalogue_functions_current_revision_fk
        FOREIGN KEY (function_id, current_function_revision_id)
        REFERENCES _orna_kernel.function_revisions(function_id, id)
        DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE _orna_kernel.definition_references (
    catalogue_revision_id bytea NOT NULL
        REFERENCES _orna_kernel.catalogue_revisions(id),
    source_function_id bytea NOT NULL CHECK (octet_length(source_function_id) = 16),
    source_function_revision_id bytea NOT NULL
        CHECK (octet_length(source_function_revision_id) = 16),
    ordinal bigint NOT NULL CHECK (ordinal >= 0),
    target_definition_id bytea NOT NULL CHECK (octet_length(target_definition_id) = 16),
    target_kind text NOT NULL CHECK (target_kind IN (
        'schema',
        'object_type',
        'field',
        'function',
        'parameter',
        'expression'
    )),
    reference_kind text NOT NULL CHECK (length(reference_kind) > 0),
    source_subobject_id bytea CHECK (
        source_subobject_id IS NULL OR octet_length(source_subobject_id) = 16
    ),
    source_unit_id bytea NOT NULL CHECK (octet_length(source_unit_id) = 16),
    source_start bigint NOT NULL CHECK (source_start >= 0),
    source_end bigint NOT NULL CHECK (source_end >= source_start),
    PRIMARY KEY (
        catalogue_revision_id,
        source_function_id,
        source_function_revision_id,
        ordinal
    ),
    CONSTRAINT definition_references_catalogue_function_revision_fk
        FOREIGN KEY (
            catalogue_revision_id,
            source_function_id,
            source_function_revision_id
        )
        REFERENCES _orna_kernel.catalogue_functions(
            catalogue_revision_id,
            function_id,
            current_function_revision_id
        ),
    CONSTRAINT definition_references_function_revision_fk
        FOREIGN KEY (source_function_id, source_function_revision_id)
        REFERENCES _orna_kernel.function_revisions(function_id, id),
    CONSTRAINT definition_references_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id)
);

CREATE INDEX definition_references_target_index
    ON _orna_kernel.definition_references (target_definition_id);

CREATE INDEX definition_references_source_index
    ON _orna_kernel.definition_references (
        source_function_id,
        source_function_revision_id
    );

REVOKE ALL ON TABLE _orna_kernel.definition_references FROM PUBLIC;
