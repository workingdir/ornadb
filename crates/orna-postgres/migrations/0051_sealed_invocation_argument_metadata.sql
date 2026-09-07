-- Retain redaction-safe, declaration-ordered argument metadata for a sealed
-- invocation that passed protected admission and binding. This remains
-- private kernel evidence: it is not a public sys.InvocationArgument
-- projection and stores no recoverable argument value.
CREATE TABLE _orna_kernel.sealed_invocation_argument_metadata (
    invocation_id bytea NOT NULL,
    position bigint NOT NULL CHECK (position >= 0),
    parameter_id bytea NOT NULL CHECK (octet_length(parameter_id) = 16),
    name text NOT NULL CHECK (length(name) > 0),
    type_kind text NOT NULL CHECK (type_kind IN ('scalar', 'named', 'reference', 'value')),
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
        'duration',
        'void'
    )),
    target_type_id bytea,
    value_digest bytea NOT NULL CHECK (octet_length(value_digest) = 32),
    redacted boolean NOT NULL DEFAULT true CHECK (redacted),
    PRIMARY KEY (invocation_id, position),
    UNIQUE (invocation_id, parameter_id),
    CONSTRAINT sealed_invocation_argument_metadata_type_shape_check CHECK (
        (type_kind = 'scalar' AND scalar_type IS NOT NULL AND target_type_id IS NULL)
        OR (type_kind IN ('named', 'reference', 'value')
            AND scalar_type IS NULL
            AND target_type_id IS NOT NULL
            AND octet_length(target_type_id) = 16)
    ),
    CONSTRAINT sealed_invocation_argument_metadata_lifecycle_fk
        FOREIGN KEY (invocation_id)
        REFERENCES _orna_kernel.sealed_invocation_lifecycle(invocation_id)
);

REVOKE ALL ON TABLE _orna_kernel.sealed_invocation_argument_metadata FROM PUBLIC;
