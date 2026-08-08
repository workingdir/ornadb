CREATE SCHEMA _orna_data;

REVOKE ALL ON SCHEMA _orna_kernel FROM PUBLIC;
REVOKE ALL ON SCHEMA _orna_data FROM PUBLIC;

CREATE TABLE _orna_kernel.source_bundles (
    id bytea PRIMARY KEY CHECK (octet_length(id) = 16),
    created_at timestamp with time zone NOT NULL DEFAULT transaction_timestamp()
);

CREATE TABLE _orna_kernel.source_units (
    id bytea PRIMARY KEY CHECK (octet_length(id) = 16),
    bundle_id bytea NOT NULL REFERENCES _orna_kernel.source_bundles(id),
    ordinal bigint NOT NULL CHECK (ordinal >= 0),
    logical_path text NOT NULL CHECK (length(logical_path) > 0),
    content text NOT NULL,
    content_hash bytea NOT NULL CHECK (octet_length(content_hash) = 32),
    hash_algorithm text NOT NULL DEFAULT 'sha256' CHECK (hash_algorithm = 'sha256'),
    encoding text NOT NULL DEFAULT 'utf-8' CHECK (encoding = 'utf-8'),
    UNIQUE (bundle_id, ordinal),
    UNIQUE (bundle_id, logical_path)
);

CREATE TABLE _orna_kernel.source_revisions (
    id bytea PRIMARY KEY CHECK (octet_length(id) = 16),
    parent_source_revision_id bytea REFERENCES _orna_kernel.source_revisions(id),
    bundle_id bytea NOT NULL UNIQUE REFERENCES _orna_kernel.source_bundles(id),
    created_at timestamp with time zone NOT NULL DEFAULT transaction_timestamp(),
    CHECK (parent_source_revision_id IS NULL OR parent_source_revision_id <> id)
);

CREATE TABLE _orna_kernel.catalogue_revisions (
    id bytea PRIMARY KEY CHECK (octet_length(id) = 16),
    source_revision_id bytea NOT NULL UNIQUE REFERENCES _orna_kernel.source_revisions(id),
    parent_catalogue_revision_id bytea REFERENCES _orna_kernel.catalogue_revisions(id),
    created_at timestamp with time zone NOT NULL DEFAULT transaction_timestamp(),
    UNIQUE (id, source_revision_id),
    CHECK (parent_catalogue_revision_id IS NULL OR parent_catalogue_revision_id <> id)
);

CREATE TABLE _orna_kernel.catalogue_schemas (
    catalogue_revision_id bytea NOT NULL REFERENCES _orna_kernel.catalogue_revisions(id),
    schema_id bytea NOT NULL CHECK (octet_length(schema_id) = 16),
    name_parts text[] NOT NULL CHECK (
        cardinality(name_parts) > 0
        AND array_position(name_parts, NULL::text) IS NULL
        AND array_position(name_parts, '') IS NULL
    ),
    PRIMARY KEY (catalogue_revision_id, schema_id),
    UNIQUE (catalogue_revision_id, name_parts)
);

CREATE TABLE _orna_kernel.catalogue_object_types (
    catalogue_revision_id bytea NOT NULL REFERENCES _orna_kernel.catalogue_revisions(id),
    type_id bytea NOT NULL CHECK (octet_length(type_id) = 16),
    schema_id bytea NOT NULL CHECK (octet_length(schema_id) = 16),
    name_parts text[] NOT NULL CHECK (
        cardinality(name_parts) >= 2
        AND array_position(name_parts, NULL::text) IS NULL
        AND array_position(name_parts, '') IS NULL
    ),
    PRIMARY KEY (catalogue_revision_id, type_id),
    UNIQUE (catalogue_revision_id, name_parts),
    FOREIGN KEY (catalogue_revision_id, schema_id)
        REFERENCES _orna_kernel.catalogue_schemas(catalogue_revision_id, schema_id)
);

CREATE TABLE _orna_kernel.catalogue_expressions (
    catalogue_revision_id bytea NOT NULL REFERENCES _orna_kernel.catalogue_revisions(id),
    expression_id bytea NOT NULL CHECK (octet_length(expression_id) = 16),
    format text NOT NULL CHECK (length(format) > 0),
    format_version integer NOT NULL CHECK (format_version > 0),
    payload bytea NOT NULL,
    content_hash bytea NOT NULL CHECK (octet_length(content_hash) = 32),
    hash_algorithm text NOT NULL DEFAULT 'sha256' CHECK (hash_algorithm = 'sha256'),
    PRIMARY KEY (catalogue_revision_id, expression_id)
);

CREATE TABLE _orna_kernel.catalogue_fields (
    catalogue_revision_id bytea NOT NULL,
    owner_type_id bytea NOT NULL CHECK (octet_length(owner_type_id) = 16),
    field_id bytea NOT NULL CHECK (octet_length(field_id) = 16),
    name text NOT NULL CHECK (length(name) > 0),
    ordinal bigint NOT NULL CHECK (ordinal >= 0),
    type_kind text NOT NULL CHECK (type_kind IN ('scalar', 'named', 'reference')),
    scalar_type text CHECK (scalar_type IN (
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
    target_type_id bytea CHECK (
        target_type_id IS NULL OR octet_length(target_type_id) = 16
    ),
    nullable boolean NOT NULL,
    is_unique boolean NOT NULL,
    default_expression_id bytea CHECK (
        default_expression_id IS NULL OR octet_length(default_expression_id) = 16
    ),
    on_delete text CHECK (on_delete IN ('restrict', 'set_null', 'cascade')),
    PRIMARY KEY (catalogue_revision_id, field_id),
    UNIQUE (catalogue_revision_id, owner_type_id, name),
    UNIQUE (catalogue_revision_id, owner_type_id, ordinal),
    FOREIGN KEY (catalogue_revision_id, owner_type_id)
        REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id),
    FOREIGN KEY (catalogue_revision_id, target_type_id)
        REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (catalogue_revision_id, default_expression_id)
        REFERENCES _orna_kernel.catalogue_expressions(catalogue_revision_id, expression_id),
    CHECK (
        (type_kind = 'scalar' AND scalar_type IS NOT NULL AND target_type_id IS NULL)
        OR (type_kind IN ('named', 'reference') AND scalar_type IS NULL AND target_type_id IS NOT NULL)
    ),
    CHECK (on_delete IS NULL OR type_kind = 'reference'),
    CHECK (on_delete <> 'set_null' OR nullable)
);

CREATE TABLE _orna_kernel.catalogue_functions (
    catalogue_revision_id bytea NOT NULL REFERENCES _orna_kernel.catalogue_revisions(id),
    function_id bytea NOT NULL CHECK (octet_length(function_id) = 16),
    schema_id bytea NOT NULL CHECK (octet_length(schema_id) = 16),
    name_parts text[] NOT NULL CHECK (
        cardinality(name_parts) >= 2
        AND array_position(name_parts, NULL::text) IS NULL
        AND array_position(name_parts, '') IS NULL
    ),
    domain text NOT NULL CHECK (domain IN ('server', 'client')),
    security_mode text NOT NULL CHECK (security_mode IN ('invoker', 'definer')),
    transaction_mode text CHECK (transaction_mode IN ('atomic', 'read_only')),
    volatility text NOT NULL CHECK (volatility IN ('immutable', 'stable', 'volatile')),
    return_shape text NOT NULL CHECK (return_shape IN ('single', 'rows')),
    return_type_kind text CHECK (return_type_kind IN ('scalar', 'named', 'reference')),
    return_scalar_type text CHECK (return_scalar_type IN (
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
    return_target_type_id bytea CHECK (
        return_target_type_id IS NULL OR octet_length(return_target_type_id) = 16
    ),
    current_function_revision_id bytea NOT NULL CHECK (
        octet_length(current_function_revision_id) = 16
    ),
    PRIMARY KEY (catalogue_revision_id, function_id),
    UNIQUE (catalogue_revision_id, name_parts),
    UNIQUE (catalogue_revision_id, function_id, current_function_revision_id),
    FOREIGN KEY (catalogue_revision_id, schema_id)
        REFERENCES _orna_kernel.catalogue_schemas(catalogue_revision_id, schema_id),
    FOREIGN KEY (catalogue_revision_id, return_target_type_id)
        REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED,
    CHECK (domain = 'server' OR transaction_mode IS NULL),
    CHECK (
        (return_shape = 'rows'
            AND return_type_kind IS NULL
            AND return_scalar_type IS NULL
            AND return_target_type_id IS NULL)
        OR (return_shape = 'single' AND (
            (return_type_kind = 'scalar'
                AND return_scalar_type IS NOT NULL
                AND return_target_type_id IS NULL)
            OR (return_type_kind IN ('named', 'reference')
                AND return_scalar_type IS NULL
                AND return_target_type_id IS NOT NULL)
        ))
    )
);

CREATE TABLE _orna_kernel.catalogue_function_parameters (
    catalogue_revision_id bytea NOT NULL,
    function_id bytea NOT NULL CHECK (octet_length(function_id) = 16),
    parameter_id bytea NOT NULL CHECK (octet_length(parameter_id) = 16),
    name text NOT NULL CHECK (length(name) > 0),
    ordinal bigint NOT NULL CHECK (ordinal >= 0),
    type_kind text NOT NULL CHECK (type_kind IN ('scalar', 'named', 'reference')),
    scalar_type text CHECK (scalar_type IN (
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
    target_type_id bytea CHECK (
        target_type_id IS NULL OR octet_length(target_type_id) = 16
    ),
    default_expression_id bytea CHECK (
        default_expression_id IS NULL OR octet_length(default_expression_id) = 16
    ),
    PRIMARY KEY (catalogue_revision_id, parameter_id),
    UNIQUE (catalogue_revision_id, function_id, name),
    UNIQUE (catalogue_revision_id, function_id, ordinal),
    FOREIGN KEY (catalogue_revision_id, function_id)
        REFERENCES _orna_kernel.catalogue_functions(catalogue_revision_id, function_id),
    FOREIGN KEY (catalogue_revision_id, target_type_id)
        REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY (catalogue_revision_id, default_expression_id)
        REFERENCES _orna_kernel.catalogue_expressions(catalogue_revision_id, expression_id),
    CHECK (
        (type_kind = 'scalar' AND scalar_type IS NOT NULL AND target_type_id IS NULL)
        OR (type_kind IN ('named', 'reference') AND scalar_type IS NULL AND target_type_id IS NOT NULL)
    )
);

CREATE TABLE _orna_kernel.catalogue_function_return_columns (
    catalogue_revision_id bytea NOT NULL,
    function_id bytea NOT NULL CHECK (octet_length(function_id) = 16),
    name text NOT NULL CHECK (length(name) > 0),
    ordinal bigint NOT NULL CHECK (ordinal >= 0),
    type_kind text NOT NULL CHECK (type_kind IN ('scalar', 'named', 'reference')),
    scalar_type text CHECK (scalar_type IN (
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
    target_type_id bytea CHECK (
        target_type_id IS NULL OR octet_length(target_type_id) = 16
    ),
    PRIMARY KEY (catalogue_revision_id, function_id, ordinal),
    UNIQUE (catalogue_revision_id, function_id, name),
    FOREIGN KEY (catalogue_revision_id, function_id)
        REFERENCES _orna_kernel.catalogue_functions(catalogue_revision_id, function_id),
    FOREIGN KEY (catalogue_revision_id, target_type_id)
        REFERENCES _orna_kernel.catalogue_object_types(catalogue_revision_id, type_id)
        DEFERRABLE INITIALLY DEFERRED,
    CHECK (
        (type_kind = 'scalar' AND scalar_type IS NOT NULL AND target_type_id IS NULL)
        OR (type_kind IN ('named', 'reference') AND scalar_type IS NULL AND target_type_id IS NOT NULL)
    )
);

CREATE TABLE _orna_kernel.function_revisions (
    id bytea PRIMARY KEY CHECK (octet_length(id) = 16),
    catalogue_revision_id bytea NOT NULL,
    function_id bytea NOT NULL CHECK (octet_length(function_id) = 16),
    revision_number bigint NOT NULL CHECK (revision_number > 0),
    content_hash bytea NOT NULL CHECK (octet_length(content_hash) = 32),
    semantic_ir_hash bytea NOT NULL CHECK (octet_length(semantic_ir_hash) = 32),
    hash_algorithm text NOT NULL DEFAULT 'sha256' CHECK (hash_algorithm = 'sha256'),
    language_version text NOT NULL CHECK (length(language_version) > 0),
    status text NOT NULL CHECK (status IN ('candidate', 'active', 'retired', 'invalid')),
    created_at timestamp with time zone NOT NULL DEFAULT transaction_timestamp(),
    UNIQUE (function_id, revision_number),
    UNIQUE (function_id, content_hash),
    UNIQUE (catalogue_revision_id, function_id, id),
    FOREIGN KEY (catalogue_revision_id, function_id)
        REFERENCES _orna_kernel.catalogue_functions(catalogue_revision_id, function_id)
        DEFERRABLE INITIALLY DEFERRED
);

ALTER TABLE _orna_kernel.catalogue_functions
    ADD CONSTRAINT catalogue_functions_current_revision_fk
    FOREIGN KEY (catalogue_revision_id, function_id, current_function_revision_id)
    REFERENCES _orna_kernel.function_revisions(catalogue_revision_id, function_id, id)
    DEFERRABLE INITIALLY DEFERRED;

CREATE TABLE _orna_kernel.function_artifacts (
    function_revision_id bytea NOT NULL REFERENCES _orna_kernel.function_revisions(id),
    artifact_kind text NOT NULL CHECK (artifact_kind IN ('server_plan', 'client_bytecode')),
    format text NOT NULL CHECK (length(format) > 0),
    format_version integer NOT NULL CHECK (format_version > 0),
    payload bytea NOT NULL,
    content_hash bytea NOT NULL CHECK (octet_length(content_hash) = 32),
    hash_algorithm text NOT NULL DEFAULT 'sha256' CHECK (hash_algorithm = 'sha256'),
    PRIMARY KEY (function_revision_id, artifact_kind)
);

CREATE TABLE _orna_kernel.active_revision (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    source_revision_id bytea NOT NULL CHECK (octet_length(source_revision_id) = 16),
    catalogue_revision_id bytea NOT NULL CHECK (octet_length(catalogue_revision_id) = 16),
    updated_at timestamp with time zone NOT NULL DEFAULT transaction_timestamp(),
    FOREIGN KEY (catalogue_revision_id, source_revision_id)
        REFERENCES _orna_kernel.catalogue_revisions(id, source_revision_id)
);

REVOKE ALL ON ALL TABLES IN SCHEMA _orna_kernel FROM PUBLIC;
ALTER DEFAULT PRIVILEGES IN SCHEMA _orna_kernel REVOKE ALL ON TABLES FROM PUBLIC;
ALTER DEFAULT PRIVILEGES IN SCHEMA _orna_data REVOKE ALL ON TABLES FROM PUBLIC;
