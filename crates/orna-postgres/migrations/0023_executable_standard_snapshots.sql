-- Persist the complete version-2 executable standard snapshot and the
-- invocation target authority relation.
--
-- This migration owns one separate standard-revision-keyed durable family for
-- the V2 standard catalogue functions, ordered parameters, immutable function
-- revisions, server artifacts, and ordered definition references. It does not
-- reuse application-catalogue foreign keys. A version-1 standard revision has
-- no row in any new executable relation. The migration does not execute
-- source, create an equivalent PostgreSQL function, or trust stored artifacts.
--
-- It also adds _orna_kernel.invocation_target_authorities, backfills one
-- application authority row for every historical application catalogue
-- function, validates the complete backfill including every existing
-- invocation-audit target pair, and only then replaces the application-only
-- invocation-audit target foreign key with one that references the common
-- authority relation. No old invocation-audit row is dropped, rewritten, or
-- made unrecoverable.

-- The V2 standard revision header records digest version 2.
ALTER TABLE _orna_kernel.standard_library_revisions
    DROP CONSTRAINT std_lib_rev_digest_version_check,
    ADD CONSTRAINT std_lib_rev_digest_version_check
        CHECK (digest_version IN (1, 2));

CREATE TABLE _orna_kernel.standard_function_revisions (
    standard_library_revision_id bytea NOT NULL,
    function_revision_id bytea NOT NULL,
    function_id bytea NOT NULL,
    revision_number bigint NOT NULL,
    declaration_source_unit_id bytea NOT NULL,
    declaration_source_start bigint NOT NULL,
    declaration_source_end bigint NOT NULL,
    declaration_content_hash bytea NOT NULL,
    semantic_hash bytea NOT NULL,
    semantic_hash_version smallint NOT NULL,
    language_version text NOT NULL,
    hash_contract_version smallint NOT NULL,
    created_at timestamp with time zone NOT NULL DEFAULT transaction_timestamp(),
    CONSTRAINT std_fn_revisions_pkey
        PRIMARY KEY (standard_library_revision_id, function_revision_id),
    CONSTRAINT std_fn_revisions_function_key
        UNIQUE (standard_library_revision_id, function_id, function_revision_id),
    CONSTRAINT std_fn_revisions_std_lib_rev_id_length
        CHECK (octet_length(standard_library_revision_id) = 16),
    CONSTRAINT std_fn_revisions_std_lib_rev_fk
        FOREIGN KEY (standard_library_revision_id)
        REFERENCES _orna_kernel.standard_library_revisions(id),
    CONSTRAINT std_fn_revisions_revision_id_length
        CHECK (octet_length(function_revision_id) = 16),
    CONSTRAINT std_fn_revisions_function_id_length
        CHECK (octet_length(function_id) = 16),
    CONSTRAINT std_fn_revisions_revision_number_check CHECK (revision_number > 0),
    CONSTRAINT std_fn_revisions_declaration_origin_check CHECK (
        octet_length(declaration_source_unit_id) = 16
        AND declaration_source_start >= 0
        AND declaration_source_start <= 4294967295
        AND declaration_source_end >= declaration_source_start
        AND declaration_source_end <= 4294967295
    ),
    CONSTRAINT std_fn_revisions_declaration_unit_fk
        FOREIGN KEY (declaration_source_unit_id)
        REFERENCES _orna_kernel.source_units(id),
    CONSTRAINT std_fn_revisions_declaration_content_hash_length
        CHECK (octet_length(declaration_content_hash) = 32),
    CONSTRAINT std_fn_revisions_semantic_hash_length
        CHECK (octet_length(semantic_hash) = 32),
    CONSTRAINT std_fn_revisions_semantic_hash_version_check
        CHECK (semantic_hash_version IN (1, 2)),
    CONSTRAINT std_fn_revisions_language_version_check
        CHECK (length(language_version) > 0),
    CONSTRAINT std_fn_revisions_contract_version_check
        CHECK (hash_contract_version > 0)
);

CREATE TABLE _orna_kernel.standard_function_artifacts (
    standard_library_revision_id bytea NOT NULL,
    function_revision_id bytea NOT NULL,
    artifact_kind text NOT NULL,
    format text NOT NULL,
    format_version integer NOT NULL,
    payload bytea NOT NULL,
    content_hash bytea NOT NULL,
    hash_algorithm text NOT NULL DEFAULT 'sha256',
    hash_contract_version smallint NOT NULL,
    CONSTRAINT std_fn_artifacts_pkey
        PRIMARY KEY (standard_library_revision_id, function_revision_id, artifact_kind),
    CONSTRAINT std_fn_artifacts_std_lib_rev_id_length
        CHECK (octet_length(standard_library_revision_id) = 16),
    CONSTRAINT std_fn_artifacts_std_lib_rev_fk
        FOREIGN KEY (standard_library_revision_id)
        REFERENCES _orna_kernel.standard_library_revisions(id),
    CONSTRAINT std_fn_artifacts_revision_id_length
        CHECK (octet_length(function_revision_id) = 16),
    CONSTRAINT std_fn_artifacts_revision_fk
        FOREIGN KEY (standard_library_revision_id, function_revision_id)
        REFERENCES _orna_kernel.standard_function_revisions(
            standard_library_revision_id,
            function_revision_id
        ),
    CONSTRAINT std_fn_artifacts_artifact_kind_check
        CHECK (artifact_kind IN ('server_plan', 'client_bytecode')),
    CONSTRAINT std_fn_artifacts_format_check
        CHECK (
            (artifact_kind = 'server_plan'
                AND format IN (
                    'orna.server-plan',
                    'orna.server-mutation-plan',
                    'orna.server-parameter-echo'
                ))
            OR (artifact_kind = 'client_bytecode' AND format = 'orna.client-plan')
        ),
    CONSTRAINT std_fn_artifacts_format_version_check CHECK (format_version > 0),
    CONSTRAINT std_fn_artifacts_content_hash_length
        CHECK (octet_length(content_hash) = 32),
    CONSTRAINT std_fn_artifacts_hash_algorithm_check
        CHECK (hash_algorithm = 'sha256'),
    CONSTRAINT std_fn_artifacts_contract_version_check
        CHECK (hash_contract_version > 0)
);

CREATE TABLE _orna_kernel.standard_catalogue_functions (
    standard_library_revision_id bytea NOT NULL,
    function_id bytea NOT NULL,
    schema_id bytea NOT NULL,
    name_parts text[] NOT NULL,
    domain text NOT NULL,
    security_mode text NOT NULL,
    transaction_mode text,
    volatility text NOT NULL,
    return_shape text NOT NULL,
    return_type_kind text,
    return_scalar_type text,
    return_value_type_id bytea,
    current_function_revision_id bytea NOT NULL,
    source_unit_id bytea NOT NULL,
    source_start bigint NOT NULL,
    source_end bigint NOT NULL,
    CONSTRAINT std_cat_functions_pkey
        PRIMARY KEY (standard_library_revision_id, function_id),
    CONSTRAINT std_cat_functions_name_key
        UNIQUE (standard_library_revision_id, name_parts),
    CONSTRAINT std_cat_functions_std_lib_rev_id_length
        CHECK (octet_length(standard_library_revision_id) = 16),
    CONSTRAINT std_cat_functions_std_lib_rev_fk
        FOREIGN KEY (standard_library_revision_id)
        REFERENCES _orna_kernel.standard_library_revisions(id),
    CONSTRAINT std_cat_functions_function_id_length
        CHECK (octet_length(function_id) = 16),
    CONSTRAINT std_cat_functions_schema_id_length
        CHECK (octet_length(schema_id) = 16),
    CONSTRAINT std_cat_functions_schema_fk
        FOREIGN KEY (standard_library_revision_id, schema_id)
        REFERENCES _orna_kernel.standard_catalogue_schemas(
            standard_library_revision_id,
            schema_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT std_cat_functions_name_parts_check CHECK (
        cardinality(name_parts) >= 2
        AND array_position(name_parts, NULL::text) IS NULL
        AND array_position(name_parts, '') IS NULL
    ),
    CONSTRAINT std_cat_functions_domain_check
        CHECK (domain IN ('server', 'client')),
    CONSTRAINT std_cat_functions_security_mode_check
        CHECK (security_mode IN ('invoker', 'definer')),
    CONSTRAINT std_cat_functions_transaction_mode_check
        CHECK (transaction_mode IS NULL OR transaction_mode IN ('atomic', 'read_only')),
    CONSTRAINT std_cat_functions_volatility_check
        CHECK (volatility IN ('immutable', 'stable', 'volatile')),
    CONSTRAINT std_cat_functions_return_shape_kind_check
        CHECK (return_shape IN ('single', 'rows')),
    CONSTRAINT std_cat_functions_return_type_kind_check
        CHECK (return_type_kind IS NULL OR return_type_kind IN ('scalar', 'value')),
    CONSTRAINT std_cat_functions_return_scalar_type_check
        CHECK (return_scalar_type IN (
            'boolean',
            'integer',
            'bigint',
            'float',
            'decimal',
            'character_large_object',
            'binary_large_object',
            'uuid',
            'date',
            'time',
            'timestamp',
            'duration',
            'void'
        )),
    CONSTRAINT std_cat_functions_return_value_type_id_length
        CHECK (return_value_type_id IS NULL OR octet_length(return_value_type_id) = 16),
    CONSTRAINT std_cat_functions_return_shape_check CHECK (
        (
            return_shape = 'rows'
            AND return_type_kind IS NULL
            AND return_scalar_type IS NULL
            AND return_value_type_id IS NULL
        )
        OR (
            return_shape = 'single'
            AND (
                (
                    return_type_kind = 'scalar'
                    AND return_scalar_type IS NOT NULL
                    AND return_value_type_id IS NULL
                )
                OR (
                    return_type_kind = 'value'
                    AND return_scalar_type IS NULL
                    AND return_value_type_id IS NOT NULL
                )
            )
        )
    ),
    CONSTRAINT std_cat_functions_current_revision_id_length
        CHECK (octet_length(current_function_revision_id) = 16),
    CONSTRAINT std_cat_functions_current_revision_fk
        FOREIGN KEY (
            standard_library_revision_id,
            function_id,
            current_function_revision_id
        )
        REFERENCES _orna_kernel.standard_function_revisions(
            standard_library_revision_id,
            function_id,
            function_revision_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT std_cat_functions_return_value_type_fk
        FOREIGN KEY (standard_library_revision_id, return_value_type_id)
        REFERENCES _orna_kernel.standard_catalogue_value_types(
            standard_library_revision_id,
            type_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT std_cat_functions_domain_transaction_check
        CHECK (domain = 'server' OR transaction_mode IS NULL),
    CONSTRAINT standard_catalogue_functions_source_origin_check CHECK (
        octet_length(source_unit_id) = 16
        AND source_start >= 0
        AND source_start <= 4294967295
        AND source_end >= source_start
        AND source_end <= 4294967295
    ),
    CONSTRAINT standard_catalogue_functions_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id)
);

CREATE TABLE _orna_kernel.standard_catalogue_function_parameters (
    standard_library_revision_id bytea NOT NULL,
    function_id bytea NOT NULL,
    parameter_id bytea NOT NULL,
    name text NOT NULL,
    ordinal bigint NOT NULL,
    type_kind text NOT NULL,
    scalar_type text,
    value_type_id bytea,
    source_unit_id bytea NOT NULL,
    source_start bigint NOT NULL,
    source_end bigint NOT NULL,
    CONSTRAINT std_cat_fn_params_pkey
        PRIMARY KEY (standard_library_revision_id, function_id, parameter_id),
    CONSTRAINT std_cat_fn_params_name_key
        UNIQUE (standard_library_revision_id, function_id, name),
    CONSTRAINT std_cat_fn_params_ordinal_key
        UNIQUE (standard_library_revision_id, function_id, ordinal),
    CONSTRAINT std_cat_fn_params_std_lib_rev_id_length
        CHECK (octet_length(standard_library_revision_id) = 16),
    CONSTRAINT std_cat_fn_params_std_lib_rev_fk
        FOREIGN KEY (standard_library_revision_id)
        REFERENCES _orna_kernel.standard_library_revisions(id),
    CONSTRAINT std_cat_fn_params_function_id_length
        CHECK (octet_length(function_id) = 16),
    CONSTRAINT std_cat_fn_params_function_fk
        FOREIGN KEY (standard_library_revision_id, function_id)
        REFERENCES _orna_kernel.standard_catalogue_functions(
            standard_library_revision_id,
            function_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT std_cat_fn_params_parameter_id_length
        CHECK (octet_length(parameter_id) = 16),
    CONSTRAINT std_cat_fn_params_name_check CHECK (length(name) > 0),
    CONSTRAINT std_cat_fn_params_ordinal_check CHECK (ordinal >= 0),
    CONSTRAINT std_cat_fn_params_type_kind_check
        CHECK (type_kind IN ('scalar', 'value')),
    CONSTRAINT std_cat_fn_params_scalar_type_check CHECK (scalar_type IN (
        'boolean',
        'integer',
        'bigint',
        'float',
        'decimal',
        'character_large_object',
        'binary_large_object',
        'uuid',
        'date',
        'time',
        'timestamp',
        'duration'
    )),
    CONSTRAINT std_cat_fn_params_value_type_id_length
        CHECK (value_type_id IS NULL OR octet_length(value_type_id) = 16),
    CONSTRAINT std_cat_fn_params_type_shape_check CHECK (
        (
            type_kind = 'scalar'
            AND scalar_type IS NOT NULL
            AND value_type_id IS NULL
        )
        OR (
            type_kind = 'value'
            AND scalar_type IS NULL
            AND value_type_id IS NOT NULL
        )
    ),
    CONSTRAINT std_cat_fn_params_value_type_fk
        FOREIGN KEY (standard_library_revision_id, value_type_id)
        REFERENCES _orna_kernel.standard_catalogue_value_types(
            standard_library_revision_id,
            type_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT standard_catalogue_function_parameters_source_origin_check CHECK (
        octet_length(source_unit_id) = 16
        AND source_start >= 0
        AND source_start <= 4294967295
        AND source_end >= source_start
        AND source_end <= 4294967295
    ),
    CONSTRAINT standard_catalogue_function_parameters_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id)
);

CREATE TABLE _orna_kernel.standard_definition_references (
    standard_library_revision_id bytea NOT NULL,
    function_revision_id bytea NOT NULL,
    ordinal bigint NOT NULL,
    target_definition_id bytea NOT NULL,
    target_kind text NOT NULL,
    target_owner_type_id bytea,
    target_owner_function_id bytea,
    target_standard_library_revision_id bytea,
    reference_kind text NOT NULL,
    source_unit_id bytea NOT NULL,
    source_start bigint NOT NULL,
    source_end bigint NOT NULL,
    CONSTRAINT std_def_references_pkey
        PRIMARY KEY (standard_library_revision_id, function_revision_id, ordinal),
    CONSTRAINT std_def_references_std_lib_rev_id_length
        CHECK (octet_length(standard_library_revision_id) = 16),
    CONSTRAINT std_def_references_std_lib_rev_fk
        FOREIGN KEY (standard_library_revision_id)
        REFERENCES _orna_kernel.standard_library_revisions(id),
    CONSTRAINT std_def_references_revision_id_length
        CHECK (octet_length(function_revision_id) = 16),
    CONSTRAINT std_def_references_revision_fk
        FOREIGN KEY (standard_library_revision_id, function_revision_id)
        REFERENCES _orna_kernel.standard_function_revisions(
            standard_library_revision_id,
            function_revision_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT std_def_references_ordinal_check CHECK (ordinal >= 0),
    CONSTRAINT std_def_references_target_definition_id_length
        CHECK (octet_length(target_definition_id) = 16),
    CONSTRAINT std_def_references_target_kind_check CHECK (target_kind IN (
        'object_type',
        'field',
        'function',
        'parameter',
        'expression',
        'value_type'
    )),
    CONSTRAINT std_def_references_target_owner_shape_check CHECK (
        (
            target_kind = 'field'
            AND target_owner_type_id IS NOT NULL
            AND target_owner_function_id IS NULL
            AND target_standard_library_revision_id IS NULL
        )
        OR (
            target_kind = 'parameter'
            AND target_owner_type_id IS NULL
            AND target_owner_function_id IS NOT NULL
            AND target_standard_library_revision_id IS NULL
        )
        OR (
            target_kind = 'value_type'
            AND target_owner_type_id IS NULL
            AND target_owner_function_id IS NULL
            AND target_standard_library_revision_id = standard_library_revision_id
        )
        OR (
            target_kind NOT IN ('field', 'parameter', 'value_type')
            AND target_owner_type_id IS NULL
            AND target_owner_function_id IS NULL
            AND target_standard_library_revision_id IS NULL
        )
    ),
    CONSTRAINT std_def_references_target_owner_type_id_length
        CHECK (target_owner_type_id IS NULL OR octet_length(target_owner_type_id) = 16),
    CONSTRAINT std_def_references_target_owner_function_id_length
        CHECK (
            target_owner_function_id IS NULL
            OR octet_length(target_owner_function_id) = 16
        ),
    CONSTRAINT std_def_references_reference_kind_check CHECK (reference_kind IN (
        'function_call',
        'named_type',
        'object_reference',
        'parameter_read',
        'query_object',
        'query_field',
        'expression',
        'write_object',
        'write_field'
    )),
    CONSTRAINT std_def_references_reference_target_compatibility_check CHECK (
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
    CONSTRAINT std_def_references_std_value_type_target_fk
        FOREIGN KEY (target_standard_library_revision_id, target_definition_id)
        REFERENCES _orna_kernel.standard_catalogue_value_types(
            standard_library_revision_id,
            type_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT std_def_references_std_parameter_target_fk
        FOREIGN KEY (
            standard_library_revision_id,
            target_owner_function_id,
            target_definition_id
        )
        REFERENCES _orna_kernel.standard_catalogue_function_parameters(
            standard_library_revision_id,
            function_id,
            parameter_id
        )
        DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT standard_definition_references_source_origin_check CHECK (
        octet_length(source_unit_id) = 16
        AND source_start >= 0
        AND source_start <= 4294967295
        AND source_end >= source_start
        AND source_end <= 4294967295
    ),
    CONSTRAINT standard_definition_references_source_unit_fk
        FOREIGN KEY (source_unit_id)
        REFERENCES _orna_kernel.source_units(id)
);

CREATE TABLE _orna_kernel.invocation_target_authorities (
    catalogue_revision_id bytea NOT NULL,
    function_id bytea NOT NULL,
    target_class text NOT NULL,
    function_revision_id bytea NOT NULL,
    standard_library_revision_id bytea,
    created_at timestamp with time zone NOT NULL DEFAULT transaction_timestamp(),
    CONSTRAINT invocation_target_authorities_pkey
        PRIMARY KEY (catalogue_revision_id, function_id),
    CONSTRAINT invocation_target_authorities_catalogue_revision_id_length
        CHECK (octet_length(catalogue_revision_id) = 16),
    CONSTRAINT invocation_target_authorities_catalogue_revision_fk
        FOREIGN KEY (catalogue_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(id),
    CONSTRAINT invocation_target_authorities_function_id_length
        CHECK (octet_length(function_id) = 16),
    CONSTRAINT invocation_target_authorities_target_class_check
        CHECK (target_class IN ('application', 'standard')),
    CONSTRAINT invocation_target_authorities_function_revision_id_length
        CHECK (octet_length(function_revision_id) = 16),
    CONSTRAINT invocation_target_authorities_std_lib_rev_id_length
        CHECK (
            standard_library_revision_id IS NULL
            OR octet_length(standard_library_revision_id) = 16
        ),
    CONSTRAINT invocation_target_authorities_class_shape_check CHECK (
        (
            target_class = 'application'
            AND standard_library_revision_id IS NULL
        )
        OR (
            target_class = 'standard'
            AND standard_library_revision_id IS NOT NULL
        )
    ),
    CONSTRAINT invocation_target_authorities_standard_pin_fk
        FOREIGN KEY (catalogue_revision_id, standard_library_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(id, standard_library_revision_id)
);

-- Backfill one application authority row for every historical application
-- catalogue function. Each row uses the function's stored current revision and
-- a null standard revision.
INSERT INTO _orna_kernel.invocation_target_authorities (
    catalogue_revision_id,
    function_id,
    target_class,
    function_revision_id,
    standard_library_revision_id
)
SELECT
    catalogue_revision_id,
    function_id,
    'application',
    current_function_revision_id,
    NULL
FROM _orna_kernel.catalogue_functions;

-- Validate the complete backfill, including every existing invocation-audit
-- target pair, before the replacement foreign key is created. A missing,
-- duplicate, or revision-mismatched application function aborts the migration.
CREATE TEMP TABLE migration_0023_authority_validation (
    is_valid boolean NOT NULL,
    CONSTRAINT migration_0023_authority_validation_check CHECK (is_valid)
) ON COMMIT DROP;

INSERT INTO migration_0023_authority_validation (is_valid)
SELECT
    NOT EXISTS (
        SELECT 1
        FROM _orna_kernel.catalogue_functions AS function
        FULL JOIN _orna_kernel.invocation_target_authorities AS authority
          ON authority.catalogue_revision_id = function.catalogue_revision_id
         AND authority.function_id = function.function_id
        WHERE authority.catalogue_revision_id IS NULL
           OR function.catalogue_revision_id IS NULL
           OR authority.target_class <> 'application'
           OR authority.function_revision_id <> function.current_function_revision_id
           OR authority.standard_library_revision_id IS NOT NULL
    )
    AND NOT EXISTS (
        SELECT 1
        FROM _orna_kernel.invocation_audit_events AS audit
        WHERE audit.function_id IS NOT NULL
          AND NOT EXISTS (
              SELECT 1
              FROM _orna_kernel.invocation_target_authorities AS authority
              WHERE authority.catalogue_revision_id = audit.catalogue_revision_id
                AND authority.function_id = audit.function_id
          )
    );

-- Replace the application-only invocation-audit target foreign key with one
-- that references the common target-authority relation. The validated
-- backfill makes this replacement lossless for every existing audit row.
ALTER TABLE _orna_kernel.invocation_audit_events
    DROP CONSTRAINT invocation_audit_events_target_fk;

ALTER TABLE _orna_kernel.invocation_audit_events
    ADD CONSTRAINT invocation_audit_events_target_fk
    FOREIGN KEY (catalogue_revision_id, function_id)
    REFERENCES _orna_kernel.invocation_target_authorities(
        catalogue_revision_id,
        function_id
    );

CREATE INDEX standard_catalogue_functions_identity_index
    ON _orna_kernel.standard_catalogue_functions (
        function_id,
        standard_library_revision_id
    );

CREATE INDEX standard_function_revisions_identity_index
    ON _orna_kernel.standard_function_revisions (
        function_revision_id,
        standard_library_revision_id
    );

CREATE INDEX standard_definition_references_revision_index
    ON _orna_kernel.standard_definition_references (
        function_revision_id,
        standard_library_revision_id,
        ordinal
    );

CREATE INDEX invocation_target_authorities_function_index
    ON _orna_kernel.invocation_target_authorities (
        function_id,
        catalogue_revision_id
    );

CREATE INDEX invocation_target_authorities_revision_index
    ON _orna_kernel.invocation_target_authorities (function_revision_id);

REVOKE ALL ON TABLE _orna_kernel.standard_function_revisions FROM PUBLIC;
REVOKE ALL ON TABLE _orna_kernel.standard_function_artifacts FROM PUBLIC;
REVOKE ALL ON TABLE _orna_kernel.standard_catalogue_functions FROM PUBLIC;
REVOKE ALL ON TABLE _orna_kernel.standard_catalogue_function_parameters FROM PUBLIC;
REVOKE ALL ON TABLE _orna_kernel.standard_definition_references FROM PUBLIC;
REVOKE ALL ON TABLE _orna_kernel.invocation_target_authorities FROM PUBLIC;
